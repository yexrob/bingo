//! Where a gateway keeps its own two files: `<data_dir>/gateway`.
//!
//! Every verb's words name the file it read, so the paths travel as one value
//! rather than as a `join` repeated in nine places.

use std::path::{Path, PathBuf};

use bingo_sdk::Env;

const DIRECTORY: &str = "gateway";
const PIDFILE: &str = "gateway.pid";
const LOG: &str = "gateway.log";

/// One data dir's gateway files.
#[derive(Clone, Debug)]
pub struct Paths {
    dir: PathBuf,
    data_dir: PathBuf,
}

impl Paths {
    pub fn new(env: &Env) -> Self {
        Self {
            dir: env.data_dir.join(DIRECTORY),
            data_dir: env.data_dir.clone(),
        }
    }

    /// The data dir the gateway is one process of: what `doctor` scans for
    /// the locks other plugins leave behind.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn pidfile(&self) -> PathBuf {
        self.dir.join(PIDFILE)
    }

    pub fn log(&self) -> PathBuf {
        self.dir.join(LOG)
    }

    /// The directory, made if this is the first gateway in this data dir.
    pub fn ensure(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_files_sit_under_the_data_dir_s_own_gateway_directory() {
        let paths = Paths::new(&Env::rooted("/home/me"));
        assert_eq!(
            paths.pidfile(),
            Path::new("/home/me/.bingo/data/gateway/gateway.pid")
        );
        assert_eq!(
            paths.log(),
            Path::new("/home/me/.bingo/data/gateway/gateway.log")
        );
        assert_eq!(paths.data_dir(), Path::new("/home/me/.bingo/data"));
    }
}
