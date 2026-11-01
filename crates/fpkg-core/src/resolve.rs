//! Resolution: PubGrub over the workspace, path packages and a registry.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyProvider, PackageResolutionStatistics, PubGrubError, Ranges,
    Reporter, VersionSet,
};

use crate::PackageId;
use crate::lockfile::{LockedPackage, Lockfile, Source};
use crate::registry::{Registry, RegistryError};
use crate::spec::{Dependency, Manifest};
use crate::version::{Comparator, Constraint, Op, Version};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no versions satisfy every constraint:\n{0}")]
    NoSolution(String),
    #[error("dependency cycle: {}; Flux's Package:include would recurse forever", .0.join(" -> "))]
    Cycle(Vec<String>),
    #[error("--offline: the cache has no metadata for {}", .0.join(", "))]
    Offline(Vec<String>),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

/// Which locked versions `update` releases.
#[derive(Clone, Debug, Default)]
pub enum Release {
    /// Keep every locked version that still fits (`fpkg lock`, `install`).
    #[default]
    None,
    /// `fpkg update`
    All,
    /// `fpkg update <id>...`
    Ids(BTreeSet<String>),
}

impl Release {
    fn releases(&self, id: &str) -> bool {
        match self {
            Release::None => false,
            Release::All => true,
            Release::Ids(ids) => ids.contains(id),
        }
    }
}

pub struct ResolveInput<'a> {
    /// The project's id and version, shown as the root of explanations.
    pub project: (&'a str, &'a Version),
    /// The project manifest's package and path dependencies.
    pub root_deps: &'a [Dependency],
    /// Workspace packages by id; each is fixed at its spec version.
    pub workspace: &'a BTreeMap<String, Manifest>,
    /// Path packages by id, with their project-relative directory.
    pub paths: &'a BTreeMap<String, (String, Manifest)>,
    pub lock: Option<&'a Lockfile>,
    pub release: Release,
    /// The registry for every other id, and its URL for `registry+<url>`.
    pub registry: Option<(&'a dyn Registry, &'a str)>,
}

/// A pubgrub package: the project or an id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Pkg {
    Root(String),
    Id(String),
}

impl fmt::Display for Pkg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pkg::Root(name) | Pkg::Id(name) => f.write_str(name),
        }
    }
}

/// A version set that remembers the constraint it was written as, so that
/// explanations say `active_ui ~> 2.0` rather than `>=2.0.0, <3.0.0`. Sets
/// computed by the solver show their ranges.
#[derive(Clone, Debug)]
struct VSet {
    ranges: Ranges<Version>,
    label: Option<String>,
}

impl PartialEq for VSet {
    fn eq(&self, other: &Self) -> bool {
        self.ranges == other.ranges
    }
}
impl Eq for VSet {}

impl fmt::Display for VSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ranges == Ranges::full() {
            return f.write_str("(any version)");
        }
        if self.ranges.is_empty() {
            return f.write_str("(no version)");
        }
        match &self.label {
            Some(l) => f.write_str(l),
            None => write!(f, "{}", self.ranges),
        }
    }
}

impl VSet {
    fn of(r: Ranges<Version>) -> Self {
        VSet { ranges: r, label: None }
    }

    /// A result equal to one operand keeps that operand's label.
    fn keep_label(&self, other: &Self, r: Ranges<Version>) -> Self {
        let label = if r == self.ranges {
            self.label.clone()
        } else if r == other.ranges {
            other.label.clone()
        } else {
            None
        };
        VSet { ranges: r, label }
    }
}

impl VersionSet for VSet {
    type V = Version;
    fn empty() -> Self {
        VSet::of(Ranges::empty())
    }
    fn singleton(v: Version) -> Self {
        VSet::of(Ranges::singleton(v))
    }
    fn complement(&self) -> Self {
        VSet::of(self.ranges.complement())
    }
    fn intersection(&self, other: &Self) -> Self {
        self.keep_label(other, self.ranges.intersection(&other.ranges))
    }
    fn contains(&self, v: &Version) -> bool {
        self.ranges.contains(v)
    }
    fn full() -> Self {
        VSet::of(Ranges::full())
    }
    fn union(&self, other: &Self) -> Self {
        self.keep_label(other, self.ranges.union(&other.ranges))
    }
}

fn comparator_ranges(c: &Comparator) -> Ranges<Version> {
    let v = c.version.clone();
    match c.op {
        Op::Eq => Ranges::singleton(v),
        Op::Gt => Ranges::strictly_higher_than(v),
        Op::Ge => Ranges::higher_than(v),
        Op::Lt => Ranges::strictly_lower_than(v),
        Op::Le => Ranges::lower_than(v),
        Op::Tilde => Ranges::between(v, c.tilde_upper()),
    }
}

/// The version ranges a constraint allows.
pub fn constraint_ranges(c: &Constraint) -> Ranges<Version> {
    c.comparators.iter().fold(Ranges::full(), |acc, cmp| acc.intersection(&comparator_ranges(cmp)))
}

fn vset(c: &Constraint) -> VSet {
    VSet { ranges: constraint_ranges(c), label: Some(c.to_string()) }
}

#[derive(Clone, Debug)]
struct Candidate {
    version: Version,
    yanked: bool,
    sha256: Option<String>,
    deps: Vec<(String, Constraint)>,
    /// A dependency that could not be read, with the reason.
    broken: Option<String>,
}

struct Provider<'a> {
    input: &'a ResolveInput<'a>,
    root_version: Version,
    candidates: RefCell<HashMap<String, Vec<Candidate>>>,
    offline_missing: RefCell<BTreeSet<String>>,
    not_found: RefCell<BTreeSet<String>>,
    /// Constraints seen on fixed (workspace/path) packages that reject them.
    fixed_conflicts: RefCell<BTreeSet<String>>,
    /// Every constraint seen on each id, for the pre-release rule.
    seen: RefCell<HashMap<String, Vec<Constraint>>>,
}

impl<'a> Provider<'a> {
    fn fixed(&self, id: &str) -> Option<&'a Manifest> {
        self.input.workspace.get(id).or_else(|| self.input.paths.get(id).map(|(_, m)| m))
    }

    fn locked(&self, id: &str) -> Option<&'a LockedPackage> {
        if self.input.release.releases(id) {
            return None;
        }
        self.input.lock.and_then(|l| l.get(id))
    }

    fn registry_candidates(&self, id: &str) -> Result<Vec<Candidate>, RegistryError> {
        if let Some(c) = self.candidates.borrow().get(id) {
            return Ok(c.clone());
        }
        let list = match (self.input.registry, PackageId::new(id)) {
            (Some((reg, _)), Ok(pid)) => match reg.package(&pid) {
                Ok(Some(doc)) => doc
                    .sorted_versions()
                    .into_iter()
                    .map(|(version, m)| {
                        let mut broken = None;
                        let deps = m
                            .dependencies
                            .iter()
                            .filter_map(|d| match Constraint::parse(&d.constraint) {
                                Ok(c) => Some((d.id.clone(), c)),
                                Err(e) => {
                                    broken = Some(format!("its dependency on {}: {e}", d.id));
                                    None
                                }
                            })
                            .collect();
                        Candidate { version, yanked: m.yanked, sha256: m.dist.as_ref().map(|d| d.sha256.clone()), deps, broken }
                    })
                    .collect(),
                Ok(None) => {
                    self.not_found.borrow_mut().insert(id.to_string());
                    Vec::new()
                }
                Err(RegistryError::Offline(_)) => {
                    self.offline_missing.borrow_mut().insert(id.to_string());
                    Vec::new()
                }
                Err(e) => return Err(e),
            },
            _ => {
                self.not_found.borrow_mut().insert(id.to_string());
                Vec::new()
            }
        };
        self.candidates.borrow_mut().insert(id.to_string(), list.clone());
        Ok(list)
    }

    fn dep_constraints(&self, owner: &str, deps: impl Iterator<Item = (String, Constraint)>) -> Vec<(Pkg, VSet)> {
        let mut merged: BTreeMap<String, Vec<Constraint>> = BTreeMap::new();
        for (id, c) in deps {
            if let Some(m) = self.fixed(&id)
                && !c.matches(&m.version)
            {
                self.fixed_conflicts.borrow_mut().insert(format!(
                    "{owner} wants {id} {c}, but {id} is a local package at {}",
                    m.version
                ));
            }
            self.seen.borrow_mut().entry(id.clone()).or_default().push(c.clone());
            merged.entry(id).or_default().push(c);
        }
        merged
            .into_iter()
            .map(|(id, cs)| {
                let set = if cs.len() == 1 {
                    vset(&cs[0])
                } else {
                    let ranges = cs.iter().fold(Ranges::full(), |acc, c| acc.intersection(&constraint_ranges(c)));
                    let label = cs.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");
                    VSet { ranges, label: Some(label) }
                };
                (Pkg::Id(id), set)
            })
            .collect()
    }
}

impl Provider<'_> {
    /// Fetches the metadata of every id reachable from the project, through
    /// every candidate version.
    fn walk_reachable(&self) -> Result<(), RegistryError> {
        let input = self.input;
        let mut queue: Vec<String> = input.root_deps.iter().filter_map(|d| d.package_id().map(str::to_string)).collect();
        for m in input.workspace.values().chain(input.paths.values().map(|(_, m)| m)) {
            queue.extend(m.package_deps().filter_map(|d| d.package_id().map(str::to_string)));
        }
        let mut seen = BTreeSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id.clone()) || self.fixed(&id).is_some() {
                continue;
            }
            for c in self.registry_candidates(&id)? {
                queue.extend(c.deps.into_iter().map(|(d, _)| d));
            }
        }
        Ok(())
    }
}

impl DependencyProvider for Provider<'_> {
    type P = Pkg;
    type V = Version;
    type VS = VSet;
    type Priority = (u32, Reverse<usize>);
    type M = String;
    type Err = RegistryError;

    fn prioritize(&self, package: &Pkg, _range: &VSet, stats: &PackageResolutionStatistics) -> Self::Priority {
        // Conflicting packages first, then those with few candidates.
        let n = match package {
            Pkg::Root(_) => 0,
            Pkg::Id(id) if self.fixed(id).is_some() => 1,
            Pkg::Id(id) => self.candidates.borrow().get(id).map(Vec::len).unwrap_or(usize::MAX / 2),
        };
        (stats.conflict_count(), Reverse(n))
    }

    fn choose_version(&self, package: &Pkg, range: &VSet) -> Result<Option<Version>, RegistryError> {
        let id = match package {
            Pkg::Root(_) => return Ok(Some(self.root_version.clone())),
            Pkg::Id(id) => id,
        };
        if let Some(m) = self.fixed(id) {
            return Ok(range.contains(&m.version).then(|| m.version.clone()));
        }
        let cands = self.registry_candidates(id)?;
        // A pre-release is a candidate only when a constraint on the id names
        // a pre-release of its X.Y.Z (semver, Cargo), or when the id is only
        // ever asked for as `*` and has no release at all.
        let seen = self.seen.borrow().get(id).cloned().unwrap_or_default();
        let only_any = seen.iter().all(Constraint::is_any);
        let has_release = cands.iter().any(|c| !c.yanked && !c.version.is_prerelease());
        let pre_ok = |v: &Version| seen.iter().any(|c| c.allows_prerelease_of(v)) || (only_any && !has_release);
        let allowed = |v: &Version| range.contains(v) && (!v.is_prerelease() || pre_ok(v));
        let locked = self.locked(id).filter(|l| matches!(l.source, Source::Registry(_))).map(|l| &l.version);
        // The locked version first (yanked or not: a yank never breaks a lock).
        if let Some(lv) = locked
            && allowed(lv)
            && cands.iter().any(|c| &c.version == lv)
        {
            return Ok(Some(lv.clone()));
        }
        // Then the newest non-yanked release, then the newest allowed pre-release.
        let ok = |c: &&Candidate| !c.yanked && allowed(&c.version);
        let pick = cands
            .iter()
            .filter(ok)
            .find(|c| !c.version.is_prerelease())
            .or_else(|| cands.iter().find(ok))
            .map(|c| c.version.clone());
        Ok(pick)
    }

    fn get_dependencies(&self, package: &Pkg, version: &Version) -> Result<Dependencies<Pkg, VSet, String>, RegistryError> {
        let input = self.input;
        let deps: Vec<(Pkg, VSet)> = match package {
            Pkg::Root(name) => {
                let owner = format!("{name} {version}");
                let mut list: Vec<(String, Constraint)> = input
                    .root_deps
                    .iter()
                    .filter_map(|d| d.package_id().map(|id| (id.to_string(), d.constraint.clone())))
                    .collect();
                // Every local package is part of the project, depended on or not.
                for id in input.workspace.keys().chain(input.paths.keys()) {
                    if !list.iter().any(|(d, _)| d == id) {
                        list.push((id.clone(), Constraint::any()));
                    }
                }
                self.dep_constraints(&owner, list.into_iter())
            }
            Pkg::Id(id) => {
                if let Some(m) = self.fixed(id) {
                    let owner = format!("{id} {version}");
                    let list = m.package_deps().filter_map(|d| d.package_id().map(|i| (i.to_string(), d.constraint.clone())));
                    self.dep_constraints(&owner, list)
                } else {
                    let cands = self.registry_candidates(id)?;
                    let Some(c) = cands.iter().find(|c| &c.version == version) else {
                        return Ok(Dependencies::Unavailable("its metadata is missing".into()));
                    };
                    if let Some(reason) = &c.broken {
                        return Ok(Dependencies::Unavailable(reason.clone()));
                    }
                    self.dep_constraints(&format!("{id} {version}"), c.deps.clone().into_iter())
                }
            }
        };
        // Pubgrub cannot express a package depending on itself; that is a cycle.
        Ok(Dependencies::Available(deps.into_iter().filter(|(p, _)| p != package).collect()))
    }
}

/// Resolves the project and returns the new lockfile.
pub fn resolve(input: &ResolveInput<'_>) -> Result<Lockfile, ResolveError> {
    // A package listing itself is the shortest cycle.
    for (id, m) in input.workspace.iter().chain(input.paths.iter().map(|(k, (_, m))| (k, m))) {
        if m.package_deps().any(|d| d.package_id() == Some(id.as_str())) {
            return Err(ResolveError::Cycle(vec![id.clone(), id.clone()]));
        }
    }
    let provider = Provider {
        input,
        root_version: input.project.1.clone(),
        candidates: RefCell::new(HashMap::new()),
        offline_missing: RefCell::new(BTreeSet::new()),
        not_found: RefCell::new(BTreeSet::new()),
        fixed_conflicts: RefCell::new(BTreeSet::new()),
        seen: RefCell::new(HashMap::new()),
    };
    let root = Pkg::Root(input.project.0.to_string());
    let solution = match pubgrub::resolve(&provider, root.clone(), input.project.1.clone()) {
        Ok(s) => s,
        Err(PubGrubError::NoSolution(mut tree)) => {
            if !provider.offline_missing.borrow().is_empty() {
                // The solver stops at the first gap; walk everything reachable so
                // that the message lists every id the cache lacks.
                provider.walk_reachable()?;
                let missing = provider.offline_missing.borrow();
                return Err(ResolveError::Offline(missing.iter().cloned().collect()));
            }
            tree.collapse_no_versions();
            let mut text = String::new();
            for line in provider.fixed_conflicts.borrow().iter() {
                text += line;
                text.push('\n');
            }
            for id in provider.not_found.borrow().iter() {
                match input.registry {
                    Some((_, url)) => text += &format!("{id} is not a local package and not on the registry {url}\n"),
                    None => text += &format!("{id} is not a local package and no registry is configured\n"),
                }
            }
            text += &DefaultStringReporter::report(&tree);
            return Err(ResolveError::NoSolution(text));
        }
        Err(PubGrubError::ErrorChoosingVersion { source, .. })
        | Err(PubGrubError::ErrorRetrievingDependencies { source, .. })
        | Err(PubGrubError::ErrorInShouldCancel(source)) => return Err(ResolveError::Registry(source)),
    };

    let mut packages = Vec::new();
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (pkg, version) in solution.iter() {
        let Pkg::Id(id) = pkg else { continue };
        let (source, sha256, deps): (Source, Option<String>, Vec<String>) = if let Some(m) = input.workspace.get(id) {
            (Source::Workspace, None, m.package_deps().filter_map(|d| d.package_id().map(str::to_string)).collect())
        } else if let Some((rel, m)) = input.paths.get(id) {
            (Source::Path(rel.clone()), None, m.package_deps().filter_map(|d| d.package_id().map(str::to_string)).collect())
        } else {
            let cands = provider.candidates.borrow();
            let c = cands.get(id).and_then(|l| l.iter().find(|c| &c.version == version));
            let Some(c) = c else {
                return Err(ResolveError::NoSolution(format!("{id} {version} vanished from the registry metadata")));
            };
            let Some(sha) = c.sha256.clone() else {
                return Err(ResolveError::Registry(RegistryError::Invalid(format!("{id} {version} has no dist.sha256"))));
            };
            let url = input.registry.map(|(_, u)| u.to_string()).unwrap_or_default();
            (Source::Registry(url), Some(sha), c.deps.iter().map(|(d, _)| d.clone()).collect())
        };
        let mut deps = deps;
        deps.sort();
        deps.dedup();
        graph.insert(id.clone(), deps.clone());
        packages.push(LockedPackage { id: id.clone(), version: version.clone(), source, sha256, dependencies: deps });
    }
    if let Some(cycle) = find_cycle(&graph) {
        return Err(ResolveError::Cycle(cycle));
    }
    Ok(Lockfile::new(packages))
}

/// The first cycle in a dependency graph, as a path that ends where it starts.
pub fn find_cycle(graph: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Active,
        Done,
    }
    fn visit<'a>(
        n: &'a str,
        graph: &'a BTreeMap<String, Vec<String>>,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(n) {
            Some(Mark::Done) => return None,
            Some(Mark::Active) => {
                let start = stack.iter().position(|x| *x == n).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..].iter().map(|s| s.to_string()).collect();
                cycle.push(n.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(n, Mark::Active);
        stack.push(n);
        for d in graph.get(n).into_iter().flatten() {
            if let Some(c) = visit(d, graph, marks, stack) {
                return Some(c);
            }
        }
        stack.pop();
        marks.insert(n, Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    for n in graph.keys() {
        let mut stack = Vec::new();
        if let Some(c) = visit(n, graph, &mut marks, &mut stack) {
            return Some(c);
        }
    }
    None
}

/// Ids in an order where every package comes after its dependencies (for
/// `publish --all`). Fails on a cycle.
pub fn topological(graph: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>, ResolveError> {
    if let Some(c) = find_cycle(graph) {
        return Err(ResolveError::Cycle(c));
    }
    fn visit(n: &str, graph: &BTreeMap<String, Vec<String>>, seen: &mut BTreeSet<String>, out: &mut Vec<String>) {
        if !seen.insert(n.to_string()) {
            return;
        }
        for d in graph.get(n).into_iter().flatten() {
            visit(d, graph, seen, out);
        }
        if graph.contains_key(n) {
            out.push(n.to_string());
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for n in graph.keys() {
        visit(n, graph, &mut seen, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
