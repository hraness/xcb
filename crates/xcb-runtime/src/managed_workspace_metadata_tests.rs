use super::*;

#[cfg(unix)]
fn fifo(path: &Path) {
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn metadata_reads_regular_bounded_utf8_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config");
    fs::write(&path, "abcd").unwrap();
    assert_eq!(bounded_read(&path, 4).as_deref(), Some("abcd"));
    assert!(bounded_read(&path, 3).is_none());
    assert!(bounded_read(temp.path(), 1024).is_none());
    assert!(bounded_read(&temp.path().join("missing"), 1024).is_none());
    fs::write(&path, [0xff]).unwrap();
    assert!(bounded_read(&path, 4).is_none());
    fs::write(&path, []).unwrap();
    assert_eq!(bounded_read(&path, 0).as_deref(), Some(""));
}

#[test]
fn metadata_rejects_replacement_between_path_check_and_open() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config");
    fs::write(&path, "first").unwrap();
    let original = crate::os::lstat(&path).unwrap();
    fs::rename(&path, temp.path().join("retained-original")).unwrap();
    fs::write(&path, "other").unwrap();
    assert!(read_metadata_file(&path, 1024, original).is_none());
    assert_eq!(bounded_read(&path, 1024).as_deref(), Some("other"));
}

#[cfg(unix)]
#[test]
fn metadata_rejects_fifo_socket_and_symlinks_without_waiting() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let temp = tempfile::tempdir().unwrap();
    let pipe = temp.path().join("pipe");
    fifo(&pipe);
    let socket = temp.path().join("socket");
    let _listener = UnixListener::bind(&socket).unwrap();
    let regular = temp.path().join("regular");
    fs::write(&regular, "text").unwrap();
    for target in [&pipe, &regular] {
        let link = temp.path().join(if target == &pipe {
            "pipe-link"
        } else {
            "file-link"
        });
        symlink(target, &link).unwrap();
        assert!(bounded_read(&link, 1024).is_none());
    }
    let started = Instant::now();
    assert!(bounded_read(&pipe, 1024).is_none());
    assert!(bounded_read(&socket, 1024).is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn metadata_rejects_fifo_and_symlink_swapped_after_regular_path_check() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config");
    fs::write(&path, "first").unwrap();
    let original = crate::os::lstat(&path).unwrap();
    fs::rename(&path, temp.path().join("retained-original")).unwrap();
    fifo(&path);
    let started = Instant::now();
    assert!(read_metadata_file(&path, 1024, original).is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
    fs::remove_file(&path).unwrap();
    symlink(temp.path().join("retained-original"), &path).unwrap();
    assert!(read_metadata_file(&path, 1024, original).is_none());
}

#[test]
fn metadata_preserves_regular_and_linked_worktree_repository_identity() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    let gitdir = main.join(".git/worktrees/linked");
    fs::create_dir_all(&gitdir).unwrap();
    fs::write(
        main.join(".git/config"),
        "[remote \"origin\"]\n url = https://github.com/fixture/project.git\n",
    )
    .unwrap();
    assert_eq!(repo_identity(&main).as_deref(), Some("fixture/project"));
    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    fs::write(
        linked.join(".git"),
        "gitdir: ../main/.git/worktrees/linked\n",
    )
    .unwrap();
    fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    assert_eq!(repo_identity(&linked).as_deref(), Some("fixture/project"));
}

#[cfg(unix)]
#[test]
fn metadata_special_git_pointer_config_and_commondir_are_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    fifo(&repo.join(".git"));
    assert!(repo_identity(&repo).is_none());
    fs::remove_file(repo.join(".git")).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();
    fifo(&repo.join(".git/config"));
    assert!(repo_identity(&repo).is_none());
    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    fs::write(linked.join(".git"), "gitdir: ../repo/.git\n").unwrap();
    fifo(&repo.join(".git/commondir"));
    assert!(repo_identity(&linked).is_none());
}

#[test]
fn metadata_result_does_not_annotate_changed_registry_or_directory() {
    let temp = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(temp.path()).unwrap();
    let managed = ManagedStore::open(&base.join("state")).unwrap();
    let path = base.join("work");
    fs::create_dir(&path).unwrap();
    managed.admit_workspace(&path, "command", None).unwrap();
    let row = registry_rows(&managed.db().unwrap()).unwrap().remove(0);
    let directory = crate::os::lstat(&path).unwrap();
    for update in [
        "UPDATE workspaces SET hidden=1",
        "UPDATE workspaces SET hidden=0, name='changed'",
        "UPDATE workspaces SET name='work', last_used=last_used+1",
        "DELETE FROM workspaces",
    ] {
        managed.write_db().unwrap().execute_batch(update).unwrap();
        managed
            .save_repo_identity(&row, directory, "fixture/stale")
            .unwrap();
        let count: i64 = managed
            .db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM workspaces WHERE repo IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    managed.admit_workspace(&path, "command", None).unwrap();
    let row = registry_rows(&managed.db().unwrap()).unwrap().remove(0);
    fs::rename(&path, base.join("retained-work")).unwrap();
    fs::create_dir(&path).unwrap();
    managed
        .save_repo_identity(&row, directory, "fixture/stale")
        .unwrap();
    assert!(
        registry_rows(&managed.db().unwrap()).unwrap()[0]
            .repo
            .is_none()
    );
    managed
        .save_repo_identity(&row, crate::os::lstat(&path).unwrap(), "fixture/current")
        .unwrap();
    assert_eq!(
        registry_rows(&managed.db().unwrap()).unwrap()[0]
            .repo
            .as_deref(),
        Some("fixture/current")
    );
}
