use super::*;

use fpkg_core::spec::{Env, SpecLocation};

fn vars(id: &str, title: &str) -> Vars {
    Vars {
        id: id.into(),
        title: title.into(),
        author: "Jane Doe".into(),
        date: "2026-11-18".into(),
        flux: DEFAULT_FLUX.into(),
        docker: false,
    }
}

/// A title and an author with every character that needs quoting somewhere.
fn tricky(id: &str) -> Vars {
    Vars {
        id: id.into(),
        title: "O'Neil's \"Big\": RP".into(),
        author: "Jo \"JD\" O'Hara: Dev".into(),
        date: "2026-11-18".into(),
        flux: ">= 0.9.1".into(),
        docker: false,
    }
}

fn paths(files: &[(String, String)]) -> Vec<&str> {
    files.iter().map(|(p, _)| p.as_str()).collect()
}

fn file<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
    files
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, t)| t.as_str())
        .unwrap_or_else(|| panic!("no {path} in {:?}", paths(files)))
}

/// Every file list `fpkg new` can produce.
fn every_list(v: &Vars) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    for minimal in [false, true] {
        for docker in [false, true] {
            out.push((
                format!("schema minimal={minimal} docker={docker}"),
                schema_files(v, minimal, docker),
            ));
        }
        out.push((
            format!("package minimal={minimal}"),
            package_files(v, minimal, false),
        ));
        out.push((
            format!("standalone package minimal={minimal}"),
            package_files(v, minimal, true),
        ));
        out.push((
            format!("plugin minimal={minimal}"),
            plugin_files(v, minimal),
        ));
        out.push((format!("addon minimal={minimal}"), addon_files(v, minimal)));
    }
    out
}

#[test]
fn file_lists() {
    let v = vars("demo", "Demo");
    assert_eq!(
        paths(&schema_files(&v, false, false)),
        vec![
            "demo.txt",
            "packagespec.lua",
            "gamemode/init.lua",
            "gamemode/cl_init.lua",
            "demo.yml",
            ".gitignore",
            ".proofreader.yml",
            "README.md",
            "schema/sh_schema.lua",
            "schema/cl_hooks.lua",
            "schema/sv_hooks.lua",
            "schema/factions/sh_citizen.lua",
            "schema/languages/en.yml",
            "schema/themes/cl_demo.lua",
        ]
    );
    let with_docker = schema_files(&v, false, true);
    assert_eq!(
        paths(&with_docker)[..14],
        paths(&schema_files(&v, false, false))[..]
    );
    assert_eq!(
        paths(&with_docker)[14..],
        ["Dockerfile", "docker-compose.yml", ".dockerignore"]
    );
    for docker in [false, true] {
        assert_eq!(
            paths(&schema_files(&v, true, docker)),
            vec![
                "demo.txt",
                "packagespec.lua",
                "gamemode/init.lua",
                "gamemode/cl_init.lua",
                "schema/sh_schema.lua"
            ]
        );
    }
    assert_eq!(
        paths(&package_files(&v, false, false)),
        vec!["packagespec.lua", "lib/sh_demo.lua"]
    );
    assert_eq!(
        paths(&package_files(&v, false, true)),
        vec![
            "packagespec.lua",
            "lib/sh_demo.lua",
            "README.md",
            ".gitignore"
        ]
    );
    assert_eq!(
        paths(&package_files(&v, true, false)),
        vec!["packagespec.lua", "lib/sh_demo.lua"]
    );
    assert_eq!(
        paths(&package_files(&v, true, true)),
        vec!["packagespec.lua", "lib/sh_demo.lua"]
    );
    assert_eq!(
        paths(&plugin_files(&v, false)),
        vec![
            "demo/plugin.yml",
            "demo/plugin/sh_plugin.lua",
            "demo/plugin/cl_hooks.lua",
            "demo/plugin/sv_hooks.lua",
            "demo/plugin/config/config.yml",
            "demo/plugin/languages/en.yml",
        ]
    );
    assert_eq!(paths(&plugin_files(&v, true)), vec!["sh_demo.lua"]);
    assert_eq!(
        paths(&addon_files(&v, false)),
        vec![
            "packagespec.lua",
            "addon.json",
            "lua/autorun/demo.lua",
            "lua/demo/sh_demo.lua",
            "lua/demo/cl_demo.lua",
            "lua/demo/sv_demo.lua",
            "README.md",
            ".gitignore",
        ]
    );
    assert_eq!(
        paths(&addon_files(&v, true)),
        vec!["packagespec.lua", "lua/autorun/demo.lua"]
    );
}

#[test]
fn every_placeholder_is_substituted() {
    for v in [vars("demo", "Demo"), tricky("my_cool_thing")] {
        for (what, files) in every_list(&v) {
            for (path, text) in &files {
                assert!(
                    !path.contains("{{") && !path.contains("}}"),
                    "{what}: {path}"
                );
                assert!(
                    !text.contains("{{") && !text.contains("}}"),
                    "{what}: {path}:\n{text}"
                );
                assert!(!text.is_empty() && text.ends_with('\n'), "{what}: {path}");
            }
        }
    }
}

#[test]
fn names() {
    assert_eq!(pascal_case("my_plugin"), "MyPlugin");
    assert_eq!(pascal_case("a2_b_c3"), "A2BC3");
    assert_eq!(pascal_case("demo"), "Demo");
    assert_eq!(title_case("my_plugin"), "My Plugin");
    let v = vars("my_cool_plugin", "My Cool Plugin");
    assert!(
        file(
            &plugin_files(&v, false),
            "my_cool_plugin/plugin/sh_plugin.lua"
        )
        .contains("PLUGIN:set_global('MyCoolPlugin')\n")
    );
    assert!(
        file(
            &plugin_files(&v, false),
            "my_cool_plugin/plugin/sv_hooks.lua"
        )
        .contains("function MyCoolPlugin:PlayerInitialized(actor)\n")
    );
    assert!(
        file(&package_files(&v, false, false), "packagespec.lua")
            .contains("  s.global      = 'MyCoolPlugin'\n")
    );
    assert!(
        file(
            &package_files(&v, false, false),
            "lib/sh_my_cool_plugin.lua"
        )
        .contains("function MyCoolPlugin:register(id, value)\n")
    );
    assert!(
        file(
            &addon_files(&v, false),
            "lua/my_cool_plugin/sh_my_cool_plugin.lua"
        )
        .contains("MyCoolPlugin = MyCoolPlugin or {}\n")
    );
    assert!(
        file(&addon_files(&v, true), "lua/autorun/my_cool_plugin.lua")
            .contains("MyCoolPlugin = MyCoolPlugin or {}\n")
    );
    let schema = schema_files(&v, false, true);
    assert!(
        file(&schema, "schema/sh_schema.lua").contains("SCHEMA.default_theme = 'my_cool_plugin'\n")
    );
    assert!(
        file(&schema, "schema/themes/cl_my_cool_plugin.lua")
            .contains("THEME.id      = 'my_cool_plugin'\n")
    );
}

#[test]
fn render_is_one_pass() {
    let v = vars("demo", "{{id}} and {{global}}");
    assert_eq!(
        render("# {{title}} ({{id}})", &v),
        "# {{id}} and {{global}} (demo)"
    );
    assert_eq!(render("no placeholders", &v), "no placeholders");
}

#[test]
fn the_schema_spec_is_the_one_new_wrote() {
    let files = schema_files(&vars("myschema", "My Schema"), false, false);
    assert_eq!(
        file(&files, "packagespec.lua"),
        "Package:describe(function(s)\n  s.name        = 'My Schema'\n  s.version     = '0.1.0'\n  s.date        = '2026-11-18'\n  s.summary     = ''\n  s.license     = 'MIT'\n\n  s.depends('flux', '~> 0.9')\n\n  -- Uncomment the dependency of the database driver the server uses, then run `fpkg install`.\n  -- SQLite needs none. MySQL and MariaDB use mysqloo, which is not an fpkg package yet:\n  -- copy its module to garrysmod/lua/bin by hand.\n  -- s.depends 'gmsv_pg'\nend)\n"
    );
    assert_eq!(
        file(&files, "myschema.txt"),
        "\"myschema\"\n{\n  \"title\"         \"My Schema\"\n  \"base\"          \"sandbox\"\n  \"author\"        \"Jane Doe\"\n  \"description\"   \"\"\n  \"version\"       \"0.1.0\"\n  \"menusystem\"    \"1\"\n}\n"
    );
    let yml = file(&files, "myschema.yml");
    assert_eq!(
        yml,
        SCHEMA.iter().find(|(p, _)| *p == "{{id}}.yml").unwrap().1
    );
    assert!(
        yml.starts_with(
            "# The plugins of Flux that the schema loads, and the config values it sets.\n"
        ),
        "{yml}"
    );
    for part in [
        "\nprofile: roleplay\n",
        "\n# depends:\n#   - items\n#   - doors\n#   - levels\n",
        "\n# exclude:\n#   - auto_walk\n#   - third_person\n",
        "\ndepends_development:\n  - dev_hud\n  - profiler\n",
        "\ncharacters_limit: 3\nchatbox_max_message_length: 256\n# raise_gun_sprint_lowers_weapon: true\n",
    ] {
        assert!(yml.contains(part), "{part:?} not in\n{yml}");
    }
    assert!(yml.lines().all(|l| l.len() <= 100), "{yml}");
    assert_eq!(
        file(&files, ".gitignore"),
        "/packages/*\n/db/\n*.local.yml\n*.local.lua\n/content/resource/fonts/fa-*.ttf\n"
    );
    assert_eq!(
        file(
            &schema_files(&vars("myschema", "My Schema"), true, true),
            "packagespec.lua"
        ),
        file(&files, "packagespec.lua")
    );
}

#[test]
fn standalone_packages_declare_their_id() {
    for minimal in [false, true] {
        let v = vars("solo", "Solo");
        assert!(
            file(&package_files(&v, minimal, true), "packagespec.lua").starts_with(
                "Package:describe(function(s)\n  s.id          = 'solo'\n  s.name        = 'Solo'\n"
            )
        );
        assert!(!file(&package_files(&v, minimal, false), "packagespec.lua").contains("s.id"));
        assert!(
            file(&addon_files(&v, minimal), "packagespec.lua")
                .starts_with("Package:describe(function(s)\n  s.id          = 'solo'\n")
        );
    }
}

/// Compiles every Lua file with the VM of the game (GLua syntax included).
#[test]
fn lua_compiles() {
    let lua = nl::new().unwrap();
    for v in [vars("demo", "Demo"), tricky("my_cool_thing")] {
        for (what, files) in every_list(&v) {
            for (path, text) in files.iter().filter(|(p, _)| p.ends_with(".lua")) {
                if let Err(e) = lua
                    .load(text.as_str())
                    .set_name(path.as_str())
                    .into_function()
                {
                    panic!("{what}: {path} does not compile: {e}\n{text}");
                }
            }
        }
    }
}

/// The specs evaluate, and give back the title and the author as they were.
#[test]
fn specs_evaluate_to_the_inputs() {
    let v = tricky("my_cool_thing");
    let tmp = tempfile::tempdir().unwrap();
    for (what, files) in every_list(&v) {
        let Some((_, spec)) = files.iter().find(|(p, _)| p == SPEC_FILE) else {
            continue;
        };
        let dir = tmp.path().join(what.replace([' ', '='], "_")).join(&v.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SPEC_FILE), spec).unwrap();
        let loc = SpecLocation {
            project_root: dir.clone(),
            dir: dir.clone(),
            id: v.id.clone(),
            project_id: v.id.clone(),
            is_project_manifest: true,
            lua_path: format!("{}/", v.id),
        };
        let m = spec::load_here(&loc, Env::Production, &loc.spec_path())
            .unwrap_or_else(|e| panic!("{what}: {e}\n{spec}"));
        assert_eq!(m.name, v.title, "{what}");
        if spec.contains("s.authors") {
            assert_eq!(m.authors, vec![v.author.clone()], "{what}");
        }
        if what.starts_with("schema") {
            assert_eq!(
                m.dependencies[0].constraint.to_string(),
                ">= 0.9.1",
                "{what}"
            );
        }
    }
}

/// A KeyValues string runs from quote to quote: the values hold no `"`.
#[test]
fn keyvalues_are_valid() {
    let v = tricky("my_cool_thing");
    let txt = file(&schema_files(&v, false, true), "my_cool_thing.txt").to_string();
    for line in txt.lines().filter(|l| l.contains('"')) {
        let quotes = line.matches('"').count();
        assert!(quotes == 2 || quotes == 4, "{line}");
    }
    assert!(
        txt.contains("  \"title\"         \"O'Neil's 'Big': RP\"\n"),
        "{txt}"
    );
    assert!(
        txt.contains("  \"author\"        \"Jo 'JD' O'Hara: Dev\"\n"),
        "{txt}"
    );
}

/// The value of a quoted YAML scalar as YAML reads it.
fn yaml_value(s: &str) -> Option<String> {
    if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        if inner.replace("''", "").contains('\'') {
            return None;
        }
        return Some(inner.replace("''", "'"));
    }
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return None,
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

/// The value as the YAML parser of Flux reads it: from quote to the next one
/// of the same kind, with no escapes.
fn flux_yaml_value(s: &str) -> Option<String> {
    let q = s.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let inner = &s[1..];
    let end = inner.find(q)?;
    (end == inner.len() - 1).then(|| inner[..end].to_string())
}

#[test]
fn yaml_is_valid_and_reads_the_same_in_flux() {
    for v in [
        vars("demo", "Demo"),
        tricky("my_cool_thing"),
        vars("demo", "Back\\slash \"quoted\""),
        vars("demo", "All ' \" \\ : #"),
    ] {
        for (what, files) in every_list(&v) {
            for (path, text) in files.iter().filter(|(p, _)| {
                p.ends_with(".yml") && p != ".proofreader.yml" && p != "docker-compose.yml"
            }) {
                for line in text
                    .lines()
                    .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                {
                    let Some((key, value)) = line.split_once(": ") else {
                        assert!(
                            line.trim_end().ends_with(':') || line.trim_start().starts_with("- "),
                            "{what}: {path}: {line}"
                        );
                        continue;
                    };
                    assert!(!key.contains(['"', '\'']), "{what}: {path}: {line}");
                    if value.starts_with(['"', '\'']) {
                        let yaml = yaml_value(value).unwrap_or_else(|| {
                            panic!("{what}: {path}: invalid YAML scalar {value}")
                        });
                        assert_eq!(Some(yaml), flux_yaml_value(value), "{what}: {path}: {line}");
                    } else {
                        assert!(
                            !value.contains(": ")
                                && !value.contains(" #")
                                && !value
                                    .starts_with(['{', '[', '&', '*', '!', '|', '>', '@', '`']),
                            "{what}: {path}: {line}"
                        );
                    }
                }
            }
        }
    }
    let plugin = plugin_files(&tricky("p"), false);
    assert!(
        file(&plugin, "p/plugin.yml").starts_with("name: \"O'Neil's 'Big': RP\"\n"),
        "{}",
        file(&plugin, "p/plugin.yml")
    );
    assert!(
        file(&plugin, "p/plugin.yml").ends_with("author: \"Jo 'JD' O'Hara: Dev\"\n"),
        "{}",
        file(&plugin, "p/plugin.yml")
    );
    assert_eq!(yaml_value(&yaml_str("O'Neil: RP")).unwrap(), "O'Neil: RP");
    assert_eq!(
        yaml_value(&yaml_str("The \"Big\" One")).unwrap(),
        "The \"Big\" One"
    );
    assert_eq!(
        yaml_value(&yaml_str("O'Neil's \"Big\": RP")).unwrap(),
        "O'Neil's 'Big': RP"
    );
}

#[test]
fn addon_json_is_valid() {
    let v = tricky("my_addon");
    let json: serde_json::Value =
        serde_json::from_str(file(&addon_files(&v, false), "addon.json")).unwrap();
    assert_eq!(json["title"], v.title);
    assert_eq!(json["type"], "ServerContent");
    assert_eq!(json["tags"], serde_json::json!([]));
    assert_eq!(
        json["ignore"],
        serde_json::json!(["packagespec.lua", "README.md", ".git*", ".fpkgignore"])
    );
}

#[test]
fn gitignore_of_a_project() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let gi = root.join(".gitignore");
    assert_eq!(gitignore_packages(root, "x").unwrap(), Gitignore::Untouched);
    assert!(!gi.exists());
    std::fs::write(&gi, "/packages/*\n/db/").unwrap();
    assert_eq!(gitignore_packages(root, "x").unwrap(), Gitignore::Appended);
    assert_eq!(
        std::fs::read_to_string(&gi).unwrap(),
        "/packages/*\n/db/\n!/packages/x/\n"
    );
    assert_eq!(gitignore_packages(root, "x").unwrap(), Gitignore::Untouched);
    assert_eq!(gitignore_packages(root, "y").unwrap(), Gitignore::Appended);
    assert_eq!(
        std::fs::read_to_string(&gi).unwrap(),
        "/packages/*\n/db/\n!/packages/x/\n!/packages/y/\n"
    );
    for line in ["/packages/", "packages/"] {
        std::fs::write(&gi, format!("{line}\n")).unwrap();
        assert_eq!(
            gitignore_packages(root, "x").unwrap(),
            Gitignore::Ignored(line.to_string())
        );
        assert_eq!(std::fs::read_to_string(&gi).unwrap(), format!("{line}\n"));
    }
    std::fs::write(&gi, "/db/\n").unwrap();
    assert_eq!(gitignore_packages(root, "x").unwrap(), Gitignore::Untouched);
}

#[test]
fn docker_matrix() {
    let v = vars("my_rp", "My RP");
    let with = schema_files(&v, false, true);
    let without = schema_files(&v, false, false);
    let readme = file(&with, "README.md");
    let section = render(
        README_DOCKER,
        &Vars {
            docker: true,
            ..v.clone()
        },
    );
    assert!(
        section.starts_with("## Running it with Docker\n") && section.ends_with("\n\n"),
        "{section}"
    );
    assert!(!section.contains("{{"));
    assert_eq!(
        readme.replacen(&section, "", 1),
        file(&without, "README.md")
    );
    assert!(
        readme.contains("\n\n## Running it with Docker\n")
            && readme.contains("\n\n## Running it on a server without Docker\n"),
        "{readme}"
    );
    assert!(!file(&without, "README.md").contains("Docker\n\nYou need Docker"));
    assert!(file(&without, "README.md").contains(
        "A schema for the Flux framework.\n\n## Running it on a server without Docker\n"
    ));
    assert_eq!(
        file(&with, ".gitignore"),
        format!(
            "{}/docker-compose.override.yml\n",
            file(&without, ".gitignore")
        )
    );
    for name in ["Dockerfile", "docker-compose.yml", ".dockerignore"] {
        assert!(!paths(&without).contains(&name), "{name}");
    }
    let dockerfile = file(&with, "Dockerfile");
    assert!(
        dockerfile.contains("COPY --chown=steam:steam . garrysmod/gamemodes/my_rp\n"),
        "{dockerfile}"
    );
    assert!(
        dockerfile.contains(
            "ENTRYPOINT [\"./srcds_run_x64\", \"-norestart\", \"+gamemode\", \"my_rp\"]\n"
        ),
        "{dockerfile}"
    );
    let compose = file(&with, "docker-compose.yml");
    assert!(
        compose.contains("    image: my_rp\n")
            && compose.contains("      - .:/home/steam/flux_server/garrysmod/gamemodes/my_rp\n"),
        "{compose}"
    );
    assert!(!compose.contains("/home/luna") && !dockerfile.contains("/home/luna"));
    assert!(readme.contains("docker build -t my_rp ."), "{readme}");
    for minimal in [false, true] {
        for docker in [false, true] {
            let files = schema_files(&v, minimal, docker);
            assert_eq!(
                paths(&files).contains(&"Dockerfile"),
                docker && !minimal,
                "minimal={minimal} docker={docker}"
            );
        }
    }
}

/// The instructions that come before the schema are those of the development
/// image of Flux, so the two share their layers.
#[test]
fn dockerfile_shares_the_server_layers() {
    let dockerfile = file(
        &schema_files(&vars("demo", "Demo"), false, true),
        "Dockerfile",
    )
    .to_string();
    let instructions: Vec<&str> = dockerfile
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    let end = instructions
        .iter()
        .position(|l| l.starts_with("COPY --from=lua_shared"))
        .unwrap();
    assert_eq!(instructions[0], "FROM alpine/git AS deps");
    assert_eq!(
        instructions[end],
        "COPY --from=lua_shared --chown=steam:steam --chmod=755 /lua_shared/ bin/linux64/"
    );
    assert!(instructions.contains(&"FROM steamcmd/steamcmd:debian-trixie"));
    assert!(
        instructions.contains(&"ARG LUA_SHARED_URL=https://data.nighty.cloud/gmod/lua_shared.so")
    );
}
