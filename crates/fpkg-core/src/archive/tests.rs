use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::*;

fn write(p: &Path, content: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// A project with one package `demo` and every kind of excluded file.
fn demo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().to_path_buf();
    let pkg = root.join("packages/demo");
    write(&root.join("packagespec.lua"), b"Package:describe(function(s) s.version = '1.0.0' end)");
    write(&root.join(".fpkgignore"), b"*.project-ignored\n");
    write(&pkg.join("packagespec.lua"), b"Package:describe(function(s)\n  s.name = 'Demo'\n  s.version = '1.0.0'\n  s.file = 'lib/demo.lua'\nend)\n");
    write(&pkg.join("lib/demo.lua"), "-- demo library\n".repeat(20).as_bytes());
    write(&pkg.join("lib/small.lua"), b"return 1\n");
    write(&pkg.join("lib/nested/deep.lua"), b"return 2\n");
    write(&pkg.join("README.md"), b"# demo\n");
    write(&pkg.join(".gitignore"), b"build/\n*.tmp\n");
    write(&pkg.join(".fpkgignore"), b"secret.txt\n");
    for excluded in [
        ".git/HEAD",
        ".svn/entries",
        ".DS_Store",
        "lib/._demo.lua",
        ".vscode/settings.json",
        ".idea/x.xml",
        "lib/demo.lua.swp",
        "config.local.yml",
        "lib/x.local.lua",
        "lib/x.generated.lua",
        "data.db",
        "a.old",
        "b.cache",
        ".env",
        "prod.env",
        "node_modules/m/index.js",
        "target/debug/x",
        ".fpkg.toml",
        "build/out.lua",
        "x.tmp",
        "secret.txt",
        "y.project-ignored",
    ] {
        write(&pkg.join(excluded), b"excluded");
    }
    (t, root, pkg)
}

fn opts(root: &Path) -> PackOptions {
    PackOptions { mtime: None, level: 6, project_root: Some(root.to_path_buf()) }
}

#[test]
fn members_and_exclusions() {
    let (_t, root, pkg) = demo();
    let packed = pack(&pkg, "demo", &opts(&root)).unwrap();
    let names: Vec<_> = packed.members.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "demo/packagespec.lua",
            "demo/.fpkgignore",
            "demo/.gitignore",
            "demo/README.md",
            "demo/lib/demo.lua",
            "demo/lib/nested/deep.lua",
            "demo/lib/small.lua",
        ]
    );
}

#[test]
fn deterministic_bytes() {
    let (_t, root, pkg) = demo();
    let a = pack(&pkg, "demo", &opts(&root)).unwrap();
    let b = pack(&pkg, "demo", &opts(&root)).unwrap();
    assert_eq!(a.bytes, b.bytes);
    // Touch every file's mtime and permissions: the bytes do not change.
    for entry in walkdir::WalkDir::new(&pkg).into_iter().flatten() {
        if entry.file_type().is_file() {
            let f = std::fs::File::options().write(true).open(entry.path()).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86400 * 400)).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }
    let c = pack(&pkg, "demo", &opts(&root)).unwrap();
    assert_eq!(a.bytes, c.bytes);
    assert_eq!(a.sha256, c.sha256);
    assert_eq!(a.sha256, crate::sha256_hex(&a.bytes));
    // A content change does change it.
    write(&pkg.join("lib/small.lua"), b"return 3\n");
    assert_ne!(pack(&pkg, "demo", &opts(&root)).unwrap().sha256, a.sha256);
}

#[test]
fn zip_details() {
    let (_t, root, pkg) = demo();
    let packed = pack(&pkg, "demo", &opts(&root)).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(&packed.bytes)).unwrap();
    assert!(zip.comment().is_empty());
    for i in 0..zip.len() {
        let f = zip.by_index(i).unwrap();
        assert!(f.extra_data().map(|d| d.is_empty()).unwrap_or(true), "{} has extra data", f.name().unwrap());
        assert!(f.comment().is_empty());
        let t = f.last_modified().unwrap();
        assert_eq!((t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second()), (1980, 1, 1, 0, 0, 0));
        assert_eq!(f.unix_mode().unwrap() & 0o777, 0o644);
        let expected = if f.size() < 64 { CompressionMethod::Stored } else { CompressionMethod::Deflated };
        assert_eq!(f.compression(), expected, "{}", f.name().unwrap());
    }
    // SOURCE_DATE_EPOCH sets the timestamps.
    let o = PackOptions { mtime: Some(1_700_000_000), ..opts(&root) };
    let dated = pack(&pkg, "demo", &o).unwrap();
    assert_ne!(dated.bytes, packed.bytes);
    let mut zip = ZipArchive::new(Cursor::new(&dated.bytes)).unwrap();
    let t = zip.by_index(0).unwrap().last_modified().unwrap();
    assert_eq!((t.year(), t.month(), t.day()), (2023, 11, 14));
    // The tree hash ignores how it was zipped.
    assert_eq!(tree_sha256(&pkg).unwrap().len(), 64);
}

#[cfg(unix)]
#[test]
fn symlinks_are_refused() {
    let (_t, root, pkg) = demo();
    std::os::unix::fs::symlink("/etc/passwd", pkg.join("lib/link.lua")).unwrap();
    let e = pack(&pkg, "demo", &opts(&root)).unwrap_err();
    assert!(matches!(&e, ArchiveError::Symlink(p) if p.ends_with("lib/link.lua")), "{e}");
    assert!(e.to_string().contains("link.lua"));
}

#[test]
fn ratio_limit_on_pack() {
    let (_t, root, pkg) = demo();
    write(&pkg.join("zeros.bin"), &vec![0u8; 1 << 20]);
    assert!(matches!(pack(&pkg, "demo", &opts(&root)), Err(ArchiveError::Ratio { .. })));
}

#[test]
fn unpack_round_trip_and_replace() {
    let (_t, root, pkg) = demo();
    let packed = pack(&pkg, "demo", &opts(&root)).unwrap();
    let dest = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dest.path().join("demo/stale")).unwrap();
    std::fs::write(dest.path().join("demo/stale/file"), b"old").unwrap();
    unpack(&packed.bytes, "demo", &packed.sha256, dest.path(), Some("id = \"demo\"\n")).unwrap();
    let out = dest.path().join("demo");
    assert!(!out.join("stale").exists());
    assert_eq!(std::fs::read(out.join("lib/small.lua")).unwrap(), b"return 1\n");
    assert!(out.join(MARKER).is_file());
    assert!(!dest.path().join(".fpkg-tmp-demo").exists());
    // Re-packing the installed tree gives the same archive (the marker is not packed).
    let again = pack(&out, "demo", &PackOptions::default()).unwrap();
    assert_eq!(again.sha256, packed.sha256);
}

#[test]
fn checksum_mismatch_writes_nothing() {
    let (_t, root, pkg) = demo();
    let packed = pack(&pkg, "demo", &opts(&root)).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let e = unpack(&packed.bytes, "demo", &"0".repeat(64), dest.path(), None).unwrap_err();
    assert!(matches!(e, ArchiveError::Checksum { .. }));
    assert_eq!(std::fs::read_dir(dest.path()).unwrap().count(), 0);
}

fn crafted(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        zw.start_file(*name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

const SPEC: (&str, &[u8]) = ("demo/packagespec.lua", b"Package:describe(function(s) s.version = '1.0.0' end)");

fn refuses(bytes: &[u8]) -> ArchiveError {
    let dest = tempfile::tempdir().unwrap();
    let sha = crate::sha256_hex(bytes);
    std::fs::create_dir(dest.path().join("demo")).unwrap();
    std::fs::write(dest.path().join("demo/keep"), b"keep").unwrap();
    let e = unpack(bytes, "demo", &sha, dest.path(), None).unwrap_err();
    // Nothing was written and the old directory is intact.
    let names: Vec<_> = std::fs::read_dir(dest.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("demo")]);
    assert_eq!(std::fs::read(dest.path().join("demo/keep")).unwrap(), b"keep");
    e
}

#[test]
fn extraction_refuses_bad_paths() {
    for bad in [
        "../evil.lua",
        "/abs.lua",
        "demo/../../evil.lua",
        "demo/./x.lua",
        "demo\\x.lua",
        "C:/x.lua",
        "demo//x.lua",
        "demo/C:x",
        &format!("demo/{}", "a".repeat(256)),
    ] {
        let e = refuses(&crafted(&[SPEC, (bad, b"x")]));
        assert!(matches!(e, ArchiveError::BadPath { .. }), "{bad}: {e}");
    }
    let e = refuses(&crafted(&[SPEC, ("other/x.lua", b"x")]));
    assert!(matches!(e, ArchiveError::WrongPrefix { .. }), "{e}");
    let e = refuses(&crafted(&[SPEC, ("demo/a.lua", b"x"), ("demo/A.lua", b"y")]));
    assert!(matches!(e, ArchiveError::Duplicate(_)), "{e}");
    let e = refuses(&crafted(&[("demo/x.lua", b"x")]));
    assert!(matches!(e, ArchiveError::MissingSpec(_)), "{e}");
    let e = refuses(b"this is not a zip");
    assert!(matches!(e, ArchiveError::Zip(_)), "{e}");
}

#[test]
fn extraction_refuses_non_regular_entries() {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    zw.start_file(SPEC.0, SimpleFileOptions::default()).unwrap();
    zw.write_all(SPEC.1).unwrap();
    zw.add_symlink("demo/link", "/etc/passwd", SimpleFileOptions::default()).unwrap();
    let e = refuses(&zw.finish().unwrap().into_inner());
    assert!(matches!(e, ArchiveError::NonRegularEntry { .. }), "{e}");

    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    zw.start_file(SPEC.0, SimpleFileOptions::default()).unwrap();
    zw.write_all(SPEC.1).unwrap();
    zw.add_directory("demo/dir/", SimpleFileOptions::default()).unwrap();
    let e = refuses(&zw.finish().unwrap().into_inner());
    assert!(matches!(e, ArchiveError::NonRegularEntry { .. }), "{e}");
}

#[test]
fn extraction_limits() {
    // A deflated bomb: 4 MiB of zeros.
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    zw.start_file(SPEC.0, SimpleFileOptions::default()).unwrap();
    zw.write_all(SPEC.1).unwrap();
    zw.start_file("demo/bomb", SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)).unwrap();
    zw.write_all(&vec![0u8; 4 << 20]).unwrap();
    let e = refuses(&zw.finish().unwrap().into_inner());
    assert!(matches!(e, ArchiveError::Ratio { .. }), "{e}");

    let names: Vec<String> = (0..MAX_MEMBERS).map(|i| format!("demo/f{i}")).collect();
    let mut entries: Vec<(&str, &[u8])> = vec![SPEC];
    entries.extend(names.iter().map(|n| (n.as_str(), &b"x"[..])));
    let e = refuses(&crafted(&entries));
    assert!(matches!(e, ArchiveError::TooManyMembers(_)), "{e}");
}

#[test]
fn path_rules() {
    assert!(check_member_path("demo/lib/x.lua").is_ok());
    assert!(check_member_path("demo/.gitignore").is_ok());
    assert!(check_member_path("demo/lib/").is_err());
    assert!(check_member_path("").is_err());
}

#[test]
fn duplicate_member_names_are_refused() {
    // The zip writer refuses duplicates, so write two names of one length and
    // rename the second to the first in the local and central headers.
    let bytes = crafted(&[SPEC, ("demo/a.lua", b"first"), ("demo/b.lua", b"second")]);
    let mut dup = bytes.clone();
    let needle = b"demo/b.lua";
    let mut i = 0;
    while i + needle.len() <= dup.len() {
        if &dup[i..i + needle.len()] == needle {
            dup[i..i + needle.len()].copy_from_slice(b"demo/a.lua");
        }
        i += 1;
    }
    let e = refuses(&dup);
    assert!(matches!(e, ArchiveError::Duplicate(ref n) if n == "demo/a.lua"), "{e}");
    assert!(validate(&bytes, "demo").is_ok());
}

#[test]
fn nested_ignore_files_apply() {
    let (_t, root, pkg) = demo();
    write(&pkg.join("lib/nested/.gitignore"), b"*.tmpl\n");
    write(&pkg.join("lib/nested/skip.tmpl"), b"skipped");
    write(&pkg.join("lib/nested/.fpkgignore"), b"local_only.lua\n");
    write(&pkg.join("lib/nested/local_only.lua"), b"skipped");
    write(&pkg.join("lib/keep.tmpl"), b"kept: the rule is in lib/nested only");
    let names: Vec<_> = pack(&pkg, "demo", &opts(&root)).unwrap().members.into_iter().map(|m| m.path).collect();
    assert!(!names.iter().any(|n| n.ends_with("skip.tmpl") || n.ends_with("local_only.lua")), "{names:?}");
    assert!(names.contains(&"demo/lib/keep.tmpl".to_string()), "{names:?}");
    assert!(names.contains(&"demo/lib/nested/.gitignore".to_string()), "{names:?}");
}

#[test]
fn tree_hash_covers_every_file() {
    let (_t, _root, pkg) = demo();
    let base = tree_sha256(&pkg).unwrap();
    // The marker is not part of it.
    write(&pkg.join(MARKER), b"id = \"demo\"\n");
    assert_eq!(tree_sha256(&pkg).unwrap(), base);
    // Files that packing would exclude are: an installed tree has none.
    for extra in ["fresh.db", "dist/out.lua", "lib/.DS_Store", "lib/nested/new.lua"] {
        write(&pkg.join(extra), b"extra");
        assert_ne!(tree_sha256(&pkg).unwrap(), base, "{extra}");
        std::fs::remove_file(pkg.join(extra)).unwrap();
        assert_eq!(tree_sha256(&pkg).unwrap(), base, "{extra}");
    }
    std::fs::remove_file(pkg.join("README.md")).unwrap();
    assert_ne!(tree_sha256(&pkg).unwrap(), base, "a missing file changes it");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", pkg.join("link")).unwrap();
        assert!(matches!(tree_sha256(&pkg), Err(ArchiveError::Symlink(_))));
    }
}

#[test]
fn interrupted_replacement_is_restored() {
    let (_t, root, pkg) = demo();
    let packed = pack(&pkg, "demo", &opts(&root)).unwrap();
    let dest = tempfile::tempdir().unwrap();
    // A crash between the two renames left only .fpkg-old-demo.
    write(&dest.path().join(".fpkg-old-demo/packagespec.lua"), b"old");
    // A failing install must not lose it ...
    let e = unpack(&packed.bytes, "demo", &"0".repeat(64), dest.path(), None).unwrap_err();
    assert!(matches!(e, ArchiveError::Checksum { .. }));
    assert!(dest.path().join(".fpkg-old-demo").exists() || dest.path().join("demo/packagespec.lua").exists());
    // ... and a good one restores it first, then replaces it.
    unpack(&packed.bytes, "demo", &packed.sha256, dest.path(), None).unwrap();
    assert!(!dest.path().join(".fpkg-old-demo").exists());
    assert_ne!(std::fs::read(dest.path().join("demo/packagespec.lua")).unwrap(), b"old");
}
