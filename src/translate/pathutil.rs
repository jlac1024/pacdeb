//! String helpers for absolute package paths like "/usr/lib/app/x".

/// The directory containing `path`. "/x" gives "/".
pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// True when `path` is `dir` or inside it.
pub fn is_under(path: &str, dir: &str) -> bool {
    path == dir || (path.starts_with(dir) && path.as_bytes().get(dir.len()) == Some(&b'/'))
}

/// Swaps the `from` prefix of `path` for `to`, if `path` is under `from`.
pub fn replace_prefix(path: &str, from: &str, to: &str) -> Option<String> {
    is_under(path, from).then(|| format!("{to}{}", &path[from.len()..]))
}

/// Resolves `target` against `base_dir` into a normalized absolute path. ".." stops at
/// the root, as it does on a real filesystem.
pub fn resolve(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base_dir.split('/').filter(|p| !p.is_empty()).collect()
    };
    for p in target.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// A relative path that leads from directory `from_dir` to `to`.
pub fn relative(from_dir: &str, to: &str) -> String {
    let from: Vec<&str> = from_dir.split('/').filter(|p| !p.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|p| !p.is_empty()).collect();
    let common = from.iter().zip(&to_parts).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = vec![".."; from.len() - common];
    out.extend(&to_parts[common..]);
    if out.is_empty() { ".".to_string() } else { out.join("/") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents_and_basenames() {
        let cases = [("/usr/bin/x", "/usr/bin", "x"), ("/x", "/", "x"), ("/a b/c d", "/a b", "c d")];
        for (path, dir, base) in cases {
            assert_eq!((parent(path), basename(path)), (dir, base), "{path}");
        }
    }

    #[test]
    fn under() {
        let cases = [
            ("/etc/apt", "/etc/apt", true),
            ("/etc/apt/x", "/etc/apt", true),
            ("/etc/aptitude", "/etc/apt", false),
            ("/etc", "/etc/apt", false),
        ];
        for (path, dir, want) in cases {
            assert_eq!(is_under(path, dir), want, "{path} under {dir}");
        }
        assert_eq!(replace_prefix("/lib/x/y", "/lib", "/usr/lib").as_deref(), Some("/usr/lib/x/y"));
        assert_eq!(replace_prefix("/lib64/y", "/lib", "/usr/lib"), None);
    }

    #[test]
    fn resolves() {
        let cases = [
            ("/usr/bin", "../lib/app/app", "/usr/lib/app/app"),
            ("/usr/bin", "/opt/App/app", "/opt/App/app"),
            ("/lib", "../usr/lib/x.so", "/usr/lib/x.so"),
            ("/usr/bin", "./x", "/usr/bin/x"),
            ("/a", "../../../x", "/x"),
        ];
        for (base, target, want) in cases {
            assert_eq!(resolve(base, target), want, "{base} + {target}");
        }
    }

    #[test]
    fn relatives() {
        let cases = [
            ("/usr/bin", "/usr/lib/app/app", "../lib/app/app"),
            ("/usr/lib", "/usr/lib/libx.so.1", "libx.so.1"),
            ("/usr/bin", "/opt/App/run", "../../opt/App/run"),
            ("/usr/lib", "/usr/lib", "."),
            ("/", "/usr/x", "usr/x"),
        ];
        for (from, to, want) in cases {
            assert_eq!(relative(from, to), want, "{from} -> {to}");
            assert_eq!(resolve(from, &relative(from, to)), to, "round trip {from} -> {to}");
        }
    }
}
