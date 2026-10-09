use super::{Sources, Wanted, sources::Candidate};
use std::{io, path::PathBuf};

/// What the scripts print instead of candidates where the shell's own paths fit.
const PATHS: &str = "@files";

const FISH: &str = r#"# dibs's completions: dibs answers what fits where the cursor is.
function __dibs_complete
    set -l found (dibs __complete fish (commandline -opc)[2..-1] "$(commandline -ct)")
    if test "$found" = @files
        __fish_complete_path (commandline -ct)
    else
        string join \n -- $found
    end
end
complete -c dibs -f -a '(__dibs_complete)'
"#;

const ZSH: &str = r#"#compdef dibs
# dibs's completions: dibs answers what fits where the cursor is.
local -a found
found=("${(@f)$(dibs __complete zsh "${(@)words[2,CURRENT]}")}")
found=(${found:#})
if [[ ${found[1]} == @files ]]; then
  _files
elif (( ${#found} )); then
  _describe -t dibs dibs found
fi
"#;

/// A shell dibs completes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Fish,
    Zsh,
}

impl Shell {
    pub fn named(name: &str) -> Option<Shell> {
        match name {
            "fish" => Some(Shell::Fish),
            "zsh" => Some(Shell::Zsh),
            _ => None,
        }
    }

    pub fn script(self) -> &'static str {
        match self {
            Shell::Fish => FISH,
            Shell::Zsh => ZSH,
        }
    }

    /// `dibs __complete <shell> <words...>`: one candidate per line, in the shell's own form.
    pub fn complete(self, typed: &[String]) -> String {
        let wanted = Wanted::at(typed);
        if wanted == Wanted::Paths {
            return format!("{PATHS}\n");
        }
        Sources::here()
            .candidates(&wanted)
            .iter()
            .map(|c| self.line(c))
            .collect()
    }

    pub fn line(self, candidate: &Candidate) -> String {
        let about = candidate.about.replace(['\t', '\n'], " ");
        match (self, about.is_empty()) {
            (Shell::Fish, true) => format!("{}\n", candidate.value),
            (Shell::Fish, false) => format!("{}\t{about}\n", candidate.value),
            (Shell::Zsh, true) => format!("{}\n", candidate.value.replace(':', "\\:")),
            (Shell::Zsh, false) => format!("{}:{about}\n", candidate.value.replace(':', "\\:")),
        }
    }

    /// Where the shell looks for it without being told: fish's vendor completions, and for zsh a
    /// directory its `fpath` has to name.
    pub fn path(self) -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(match self {
            Shell::Fish => data.join("fish/vendor_completions.d/dibs.fish"),
            Shell::Zsh => data.join("zsh/site-functions/_dibs"),
        })
    }

    /// Writes the script where the shell finds it, and says what is left to do.
    pub fn install(self) -> io::Result<String> {
        let path = self
            .path()
            .ok_or_else(|| io::Error::other("no HOME to install into"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, self.script())?;
        let dir = path.parent().unwrap_or(&path).display();
        Ok(match self {
            Shell::Fish => format!(
                "installed {}\nA new fish completes dibs; this one does after:  complete -e dibs; source {}\n",
                path.display(),
                path.display()
            ),
            Shell::Zsh => format!(
                "installed {}\nzsh reads it once {dir} is in fpath. In ~/.zshrc, before compinit:\n  fpath=({dir} $fpath)\nthen start a new zsh, after  rm -f ~/.zcompdump*  if completions were cached.\n",
                path.display()
            ),
        })
    }
}
