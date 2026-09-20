use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

const OBJECT: &str = "1111111111111111111111111111111111111111";

struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    work: PathBuf,
    coordination: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        let coordination = private::directory(&base.join("coordination")).unwrap();
        Self {
            _temp: temp,
            base,
            work,
            coordination,
        }
    }
    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn ordinary(&self) -> PathBuf {
        let git = self.work.join(".git");
        Self::write(&git.join("HEAD"), format!("{OBJECT}\n").as_bytes());
        Self::write(&git.join("index"), &empty_index());
        Self::write(
            &git.join(format!("objects/{}/{}", &OBJECT[..2], &OBJECT[2..])),
            b"synthetic compressed object",
        );
        git
    }
    fn capture(&self) -> Result<Option<GitSnapshot>> {
        capture(
            &File::open(&self.work).unwrap(),
            &self.work,
            &self.coordination,
        )
    }
    fn linked(&self) -> (PathBuf, PathBuf) {
        let common = self.base.join("repository/.git");
        let git = common.join("worktrees/linked");
        Self::write(
            &self.work.join(".git"),
            format!("gitdir: {}\n", git.display()).as_bytes(),
        );
        Self::write(
            &git.join("gitdir"),
            format!("{}\n", self.work.join(".git").display()).as_bytes(),
        );
        Self::write(&git.join("commondir"), b"../..\n");
        Self::write(&git.join("HEAD"), format!("{OBJECT}\n").as_bytes());
        Self::write(&git.join("index"), &empty_index());
        Self::write(
            &common.join(format!("objects/{}/{}", &OBJECT[..2], &OBJECT[2..])),
            b"synthetic compressed object",
        );
        (git, common)
    }
    fn associate(&self, git: PathBuf, common: PathBuf) {
        let directory = private::directory(&self.coordination.join("git-associations")).unwrap();
        private::create(
            &directory.join(format!("{}.json", digest(self.work.to_str().unwrap()))),
            &serde_json::to_vec(&GitAssociation {
                version: 1,
                workspace: self.work.clone(),
                git_dir: git,
                common_dir: common,
            })
            .unwrap(),
        )
        .unwrap();
    }
}
fn empty_index() -> Vec<u8> {
    let mut bytes = b"DIRC".to_vec();
    bytes.extend(2u32.to_be_bytes());
    bytes.extend(0u32.to_be_bytes());
    bytes.extend([0; 20]);
    bytes
}
fn one_index(mode: u32, flags: u16, path: &str) -> Vec<u8> {
    let mut bytes = b"DIRC".to_vec();
    bytes.extend(2u32.to_be_bytes());
    bytes.extend(1u32.to_be_bytes());
    let mut entry = vec![0; 62];
    entry[24..28].copy_from_slice(&mode.to_be_bytes());
    entry[60..62].copy_from_slice(&(flags | path.len() as u16).to_be_bytes());
    entry.extend(path.as_bytes());
    entry.push(0);
    entry.resize(entry.len().div_ceil(8) * 8, 0);
    bytes.extend(entry);
    bytes.extend([0; 20]);
    bytes
}

#[test]
fn captures_only_trusted_projector_inputs_without_configuration_or_credentials() {
    let fixture = Fixture::new();
    let git = fixture.ordinary();
    for name in [
        "config",
        "config.worktree",
        "FETCH_HEAD",
        "logs/HEAD",
        "hooks/pre-commit",
        "credentials",
    ] {
        Fixture::write(&git.join(name), b"must not be captured");
    }
    Fixture::write(
        &git.join("objects/pack/pack-2222222222222222222222222222222222222222.pack"),
        b"pack",
    );
    Fixture::write(
        &git.join("objects/pack/pack-2222222222222222222222222222222222222222.idx"),
        b"index",
    );
    Fixture::write(
        &git.join("objects/pack/pack-2222222222222222222222222222222222222222.rev"),
        b"must not be captured",
    );
    let snapshot = fixture.capture().unwrap().unwrap();
    assert_eq!(snapshot.head_object_id.as_deref(), Some(OBJECT));
    assert_eq!(snapshot.files.len(), 5);
    for file in &snapshot.files {
        let bytes = STANDARD.decode(&file.base64).unwrap();
        assert_eq!(file.sha256, digest(&bytes));
        assert_ne!(bytes, b"must not be captured");
        assert!(file.path == "HEAD" || file.path == "index" || file.path.starts_with("objects/"));
    }
    let debug = format!("{snapshot:?}");
    assert!(!debug.contains(OBJECT));
    assert!(!debug.contains("synthetic"));
}

#[test]
fn resolves_loose_packed_and_unborn_heads_without_importing_remote_configuration() {
    for kind in ["loose", "packed", "unborn"] {
        let fixture = Fixture::new();
        let git = fixture.ordinary();
        Fixture::write(&git.join("HEAD"), b"ref: refs/heads/main\n");
        if kind == "loose" {
            Fixture::write(
                &git.join("refs/heads/main"),
                format!("{OBJECT}\n").as_bytes(),
            );
        }
        if kind == "packed" {
            Fixture::write(
                &git.join("packed-refs"),
                format!(
                    "# pack-refs with: peeled fully-peeled sorted \n{OBJECT} refs/heads/main\n"
                )
                .as_bytes(),
            );
        }
        assert_eq!(
            fixture
                .capture()
                .unwrap()
                .unwrap()
                .head_object_id
                .as_deref(),
            if kind == "unborn" { None } else { Some(OBJECT) }
        );
    }
}

#[test]
fn linked_worktrees_require_exact_private_association_and_reciprocal_pointers() {
    let fixture = Fixture::new();
    let (git, common) = fixture.linked();
    assert!(
        fixture
            .capture()
            .unwrap_err()
            .to_string()
            .contains("explicit trusted host association")
    );
    fixture.associate(git.clone(), common);
    let snapshot = fixture.capture().unwrap().unwrap();
    assert_eq!(snapshot.files.len(), 3);
    assert!(
        snapshot
            .files
            .iter()
            .all(|file| !matches!(file.path.as_str(), "gitdir" | "commondir"))
    );
    Fixture::write(&git.join("gitdir"), b"/unrelated/workspace/.git\n");
    assert!(fixture.capture().is_err());
    Fixture::write(
        &git.join("gitdir"),
        format!("{}\n", fixture.work.join(".git").display()).as_bytes(),
    );
    Fixture::write(&git.join("commondir"), b"../../../unrelated\n");
    assert!(fixture.capture().is_err());
}

#[test]
fn private_association_cannot_authorize_a_different_workspace_or_symlinked_root() {
    for kind in ["workspace", "symlink", "public"] {
        let fixture = Fixture::new();
        let (git, common) = fixture.linked();
        fixture.associate(git.clone(), common.clone());
        if kind == "workspace" {
            Fixture::write(&fixture.work.join(".git"), b"gitdir: /unrelated/path\n");
        }
        if kind == "symlink" {
            let real = fixture.base.join("moved");
            fs::rename(&common, &real).unwrap();
            symlink(real, &common).unwrap();
        }
        if kind == "public" {
            let file = fixture
                .coordination
                .join("git-associations")
                .join(format!("{}.json", digest(fixture.work.to_str().unwrap())));
            fs::set_permissions(file, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(fixture.capture().is_err(), "{kind}");
    }
}

#[test]
fn selected_symlinks_hardlinks_special_files_and_large_files_fail_closed() {
    for kind in ["symlink", "hardlink", "directory", "large", "gitlink"] {
        let fixture = Fixture::new();
        let git = fixture.ordinary();
        if kind == "gitlink" {
            fs::rename(&git, fixture.base.join("real-git")).unwrap();
            symlink(fixture.base.join("real-git"), &git).unwrap();
        } else {
            fs::remove_file(git.join("index")).unwrap();
            match kind {
                "symlink" => symlink(git.join("HEAD"), git.join("index")).unwrap(),
                "hardlink" => fs::hard_link(git.join("HEAD"), git.join("index")).unwrap(),
                "directory" => fs::create_dir(git.join("index")).unwrap(),
                _ => File::create(git.join("index"))
                    .unwrap()
                    .set_len(GIT_FILE_LIMIT as u64 + 1)
                    .unwrap(),
            }
        }
        assert!(fixture.capture().is_err(), "{kind}");
    }
}

#[test]
fn unsupported_indirections_and_object_names_are_rejected_without_following_them() {
    for name in [
        "objects/info/alternates",
        "objects/info/http-alternates",
        "sharedindex.1111",
        "modules/child",
        "shallow",
        "commondir",
        "objects/pack/pack-111.promisor",
        "objects/pack/pack-bad.pack",
        "objects/11/invalid",
    ] {
        let fixture = Fixture::new();
        let git = fixture.ordinary();
        Fixture::write(&git.join(name), b"/unrelated/secret");
        assert!(fixture.capture().is_err(), "{name}");
    }
}

#[test]
fn index_admission_rejects_semantics_that_cannot_be_projected_faithfully() {
    assert!(index(&empty_index()).is_ok());
    assert!(index(&one_index(0o100644, 0, "src/file")).is_ok());
    assert!(index(&one_index(0o100755, 0, "executable")).is_ok());
    for stage in [0x1000, 0x2000, 0x3000] {
        assert!(index(&one_index(0o100755, stage, "conflicted")).is_err());
    }
    for data in [
        one_index(0o160000, 0, "submodule"),
        one_index(0o120000, 0, "symlink"),
        one_index(0o100644, 0x8000, "assumed"),
        one_index(0o100644, 0x4000, "extended"),
        one_index(0o100644, 0, "../outside"),
        one_index(0o100644, 0, ".git/file"),
    ] {
        assert!(index(&data).is_err());
    }
    for extension in [b"link", b"sdir", b"FSMN"] {
        let mut bytes = empty_index();
        bytes.splice(12..12, extension.iter().copied().chain(0u32.to_be_bytes()));
        assert!(index(&bytes).is_err());
    }
    let mut bytes = empty_index();
    bytes[4..8].copy_from_slice(&4u32.to_be_bytes());
    assert!(index(&bytes).is_err());
    for length in 0..32 {
        assert!(index(&empty_index()[..length]).is_err());
    }
}

#[test]
fn source_guards_detect_post_read_mutation_and_same_content_replacement() {
    for replacement in [false, true] {
        let fixture = Fixture::new();
        let git = fixture.ordinary();
        let root = File::open(&git).unwrap();
        let owner = root.metadata().unwrap().uid();
        let mut tree = Tree::new(root, git.clone(), owner).unwrap();
        let bytes = tree.read("HEAD", 4096).unwrap();
        if replacement {
            Fixture::write(&git.join("new-head"), &bytes);
            fs::rename(git.join("new-head"), git.join("HEAD")).unwrap();
        } else {
            Fixture::write(&git.join("HEAD"), b"different bytes");
        }
        assert!(tree.verify().is_err());
    }
}

#[test]
fn absent_git_and_limits_have_explicit_bounded_behavior() {
    let fixture = Fixture::new();
    assert!(fixture.capture().unwrap().is_none());
    let mut budget = Budget::default();
    budget.add(GIT_BYTE_LIMIT).unwrap();
    assert!(budget.add(1).is_err());
    let mut budget = Budget::default();
    for _ in 0..GIT_ENTRY_LIMIT {
        budget.visit().unwrap();
    }
    assert!(budget.visit().is_err());
    for reference_name in [
        "refs/heads/../outside",
        "refs/heads/a.lock",
        "refs/heads/.hidden",
        "refs/heads/a b",
        "refs/heads/a\nb",
    ] {
        assert!(reference(reference_name).is_err());
    }
}
