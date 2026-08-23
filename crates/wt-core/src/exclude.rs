use std::collections::BTreeSet;

pub const START: &str = "# >>> wt managed >>>";
pub const END: &str = "# <<< wt managed <<<";

pub fn block(paths: impl IntoIterator<Item = String>) -> String {
    let mut paths: BTreeSet<String> = paths
        .into_iter()
        .map(|path| path.trim_start_matches('/').to_owned())
        .collect();
    paths.insert("**/.wt-tmp-*".to_owned());
    paths.insert(".wt/".to_owned());
    let mut output = format!("{START}\n");
    for path in paths {
        output.push('/');
        output.push_str(&path);
        output.push('\n');
    }
    output.push_str(END);
    output.push('\n');
    output
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Splice {
    pub text: String,
    pub repaired: bool,
}

pub fn splice(existing: &str, managed: &str) -> Splice {
    let Some(start) = existing.find(START) else {
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return Splice {
            text: format!("{existing}{separator}{managed}"),
            repaired: false,
        };
    };
    let after_start = start + START.len();
    if let Some(relative_end) = existing[after_start..].find(END) {
        let mut end = after_start + relative_end + END.len();
        if existing.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        Splice {
            text: format!("{}{}{}", &existing[..start], managed, &existing[end..]),
            repaired: false,
        }
    } else {
        Splice {
            text: format!("{}{}", &existing[..start], managed),
            repaired: true,
        }
    }
}

pub fn remove(existing: &str) -> Splice {
    let Some(start) = existing.find(START) else {
        return Splice {
            text: existing.to_owned(),
            repaired: false,
        };
    };
    let after_start = start + START.len();
    let Some(relative_end) = existing[after_start..].find(END) else {
        return Splice {
            text: existing[..start].to_owned(),
            repaired: true,
        };
    };
    let mut end = after_start + relative_end + END.len();
    if existing.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    Splice {
        text: format!("{}{}", &existing[..start], &existing[end..]),
        repaired: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_outside_bytes_and_repairs_unclosed_block() {
        let managed = block(["generated".to_owned()]);
        let existing = format!("before\n{START}\nstale\nafter without marker");
        let result = splice(&existing, &managed);
        assert!(result.repaired);
        assert_eq!(result.text, format!("before\n{managed}"));
    }

    #[test]
    fn block_is_sorted_deduplicated_and_removable() {
        let managed = block([
            "z/file".to_owned(),
            "/a/file".to_owned(),
            "z/file".to_owned(),
            ".wt/".to_owned(),
        ]);
        assert_eq!(
            managed,
            format!("{START}\n/**/.wt-tmp-*\n/.wt/\n/a/file\n/z/file\n{END}\n")
        );
        let existing = format!("before\n{managed}after\n");
        assert_eq!(remove(&existing).text, "before\nafter\n");
    }
}
