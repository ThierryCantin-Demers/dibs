//! A Cargo.lock, read for its packages: what a pin redirects, what a build claims, and what a
//! pin that did not take left where it was.

/// One `[[package]]` of a Cargo.lock. A package without a source is a path in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub source: Option<String>,
}

impl Package {
    /// Every package of a lockfile, in its order.
    ///
    /// # Assumptions
    ///
    /// - `lock` is the text of a Cargo.lock.
    ///
    /// # Guarantees
    ///
    /// - Only `[[package]]` tables are read: a `[[patch.unused]]` table or any other names no
    ///   package and changes none.
    /// - A table without a `name` is left out.
    pub fn all_in_lock(lock: &str) -> Vec<Package> {
        let mut out = Vec::new();
        let mut current: Option<Package> = None;
        for line in lock.lines().map(str::trim) {
            match line.starts_with('[') {
                true => out.extend(std::mem::replace(&mut current, Package::opened_by(line))),
                false => current
                    .iter_mut()
                    .for_each(|package| take_in(package, line)),
            }
        }
        out.extend(current);
        out.retain(|p| !p.name.is_empty());
        out
    }

    /// The package a table's header opens: an empty one for `[[package]]`, None for any other.
    fn opened_by(header: &str) -> Option<Package> {
        (header == "[[package]]").then(|| Package {
            name: String::new(),
            version: None,
            source: None,
        })
    }
}

/// Takes one `key = "value"` line of a package's table into it.
fn take_in(package: &mut Package, line: &str) {
    let Some((key, value)) = line.split_once(" = ") else {
        return;
    };
    let value = value.trim_matches('"').to_string();
    match key {
        "name" => package.name = value,
        "version" => package.version = Some(value),
        "source" => package.source = Some(value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";

    #[test]
    fn every_package_is_read_with_what_it_declares() {
        let packages = Package::all_in_lock(LOCK);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].source, None);
        assert_eq!(packages[1].version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn an_unused_patch_after_the_packages_changes_none_of_them() {
        let lock = format!(
            "{LOCK}\n[[patch.unused]]\nname = \"cubecl\"\nversion = \"0.9.0\"\nsource = \"git+https://example.com/cubecl?tag=v1#abc\"\n"
        );
        assert_eq!(Package::all_in_lock(&lock), Package::all_in_lock(LOCK));
    }
}
