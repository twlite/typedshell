#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    Bash,
    Pwsh,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TargetOs {
    Linux,
    Macos,
    Windows,
    Freebsd,
}
impl TargetOs {
    pub fn host() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Macos,
            "windows" => Self::Windows,
            "freebsd" => Self::Freebsd,
            _ => Self::Linux,
        }
    }
    pub fn is_unix(self) -> bool {
        self != Self::Windows
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Comments {
    All,
    Doc,
    None,
}
#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub target: Target,
    pub os: TargetOs,
    pub comments: Comments,
}
impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::Bash,
            os: TargetOs::host(),
            comments: Comments::All,
        }
    }
}
