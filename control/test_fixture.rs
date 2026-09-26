//! C fixture programs for unit tests. Each is compiled from a test's source
//! with the system C compiler into a private temporary directory, which is
//! removed when the fixture is dropped.
use std::path::{Path, PathBuf};

pub struct CFixture {
    root: PathBuf,
    program: PathBuf,
}

impl CFixture {
    /// Compiles `source` as C11 with warnings as errors, and with `-D<define>`
    /// for each of `defines`, into `<temp>/hamn-<name>-<pid>/program`. `name`
    /// must be unique among the fixtures that one test process holds at once.
    pub fn compile(name: &str, source: &str, defines: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!("hamn-{name}-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap_or_else(|error| panic!("{}: {error}", root.display()));
        let fixture = Self { program: root.join("program"), root };
        let file = fixture.root.join("program.c");
        std::fs::write(&file, source).unwrap();
        let output = std::process::Command::new("cc")
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
            .args(defines.iter().map(|define| format!("-D{define}")))
            .arg("-o")
            .arg(&fixture.program)
            .arg(&file)
            .output()
            .expect("a C compiler is required for fixture programs");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        fixture
    }

    pub fn program(&self) -> &Path {
        &self.program
    }
}

impl Drop for CFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
