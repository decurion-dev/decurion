//! An app's deploy steps, as the owner saves them in the app's rules.
//!
//! The steps are what one Deploy tap runs on the owner's PC, in a clean copy
//! of the app's merged main line (ADR 0002). They are plain commands, one
//! per line, each run as a program with its words, never through a shell:
//! nothing in a step can chain another command, send output somewhere, or
//! fill in a variable. Rules that must stay true:
//!
//! - **Plain programs.** A step starts with a program's name (`npm`, `node`),
//!   never a path, so it is found the usual way on the PC.
//! - **No shell marks** outside double quotes (`| & ; < > \` $( )`), and no
//!   control characters anywhere, so a step means what the owner sees.
//! - **Small.** At most [`MAX_STEPS`] steps of at most [`MAX_STEP_LEN`]
//!   characters each.
//! - **Refusals are for the owner**: short sentences that say what to change.

/// Most steps one deploy runs.
pub const MAX_STEPS: usize = 8;

/// Longest step, in characters.
pub const MAX_STEP_LEN: usize = 300;

/// Longest site address.
pub const MAX_SITE_LEN: usize = 200;

/// One step: the program, then its words. Run without a shell.
pub type Step = Vec<String>;

/// Characters that mean something to a shell. Outside double quotes they
/// are refused, so nobody can believe a step chains or redirects anything.
const SHELL_MARKS: &[char] = &['|', '&', ';', '<', '>', '`', '$', '(', ')'];

/// Reads the owner's steps: one step per line that is not empty, words split
/// on spaces, and "double quotes" keep spaces inside one word.
pub fn parse_steps(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let n = i + 1;
        if line.chars().count() > MAX_STEP_LEN {
            return Err(format!(
                "Step on line {n} is too long. Keep each step under {MAX_STEP_LEN} characters."
            ));
        }
        if line.chars().any(char::is_control) {
            return Err(format!(
                "Step on line {n} has a hidden character in it. Type it again by hand."
            ));
        }
        let step = words(line).map_err(|why| format!("Step on line {n} {why}"))?;
        if !plain_program(&step[0]) {
            return Err(format!(
                "Step on line {n} must start with a program's name, such as npm or node, not a folder path."
            ));
        }
        steps.push(step);
        if steps.len() > MAX_STEPS {
            return Err(format!(
                "There are more than {MAX_STEPS} steps. Join some into one script and run that."
            ));
        }
    }
    if steps.is_empty() {
        return Err("Add at least one step, such as npm run deploy.".into());
    }
    Ok(steps)
}

/// Splits one line into words. The error finishes the sentence "Step on
/// line N ...".
fn words(line: &str) -> Result<Step, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c if !quoted && SHELL_MARKS.contains(&c) => {
                return Err(format!(
                    "uses {c}, which Decurion does not run. Put each command on its own line instead."
                ));
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err("has a quote mark that is never closed. Add the closing \" mark.".into());
    }
    if started {
        out.push(word);
    }
    if out.is_empty() || out[0].is_empty() {
        return Err("has no program to run.".into());
    }
    Ok(out)
}

/// A program's bare name: letters, digits, `.`, `_` and `-`, not starting
/// with a mark, and never a path.
fn plain_program(p: &str) -> bool {
    (1..=40).contains(&p.len())
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && p.as_bytes()[0].is_ascii_alphanumeric()
}

/// Checks the address the app goes live at, for the Open site link. Only
/// https, with a site name, no user name or password and no spaces. Returns
/// it trimmed.
pub fn check_site(url: &str) -> Result<String, String> {
    let u = url.trim();
    let bad = || Err("Give the live address in full, starting with https://".to_owned());
    if u.chars().count() > MAX_SITE_LEN {
        return Err(format!(
            "That address is too long. Keep it under {MAX_SITE_LEN} characters."
        ));
    }
    let Some(rest) = u.strip_prefix("https://") else {
        return bad();
    };
    if u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Take the spaces out of the address.".into());
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains('@') {
        return Err("Leave the user name and password out of the address.".into());
    }
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return bad();
    }
    Ok(u.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Step {
        v.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn steps_are_read_one_per_line_without_a_shell() {
        let steps =
            parse_steps("npm ci\n\n  npm ci --prefix infra  \nnode scripts/deploy-prod.mjs\n")
                .expect("plain steps");
        assert_eq!(
            steps,
            vec![
                s(&["npm", "ci"]),
                s(&["npm", "ci", "--prefix", "infra"]),
                s(&["node", "scripts/deploy-prod.mjs"]),
            ]
        );
        let quoted = parse_steps(r#"node run.mjs "my site" --mark "a;b""#).expect("quotes");
        assert_eq!(
            quoted,
            vec![s(&["node", "run.mjs", "my site", "--mark", "a;b"])]
        );
        assert_eq!(
            parse_steps("npm run deploy:prod").expect("colon is fine"),
            vec![s(&["npm", "run", "deploy:prod"])]
        );
    }

    #[test]
    fn shell_marks_paths_and_hidden_characters_are_refused_in_plain_words() {
        for bad in [
            "npm ci && npm run deploy",
            "npm run deploy | tee log",
            "npm run deploy > out.txt",
            "echo $(whoami)",
            "echo `id`",
            "npm ci; rm -rf /",
        ] {
            let e = parse_steps(bad).expect_err(bad);
            assert!(e.contains("its own line"), "{bad}: {e}");
        }
        assert!(parse_steps("/usr/bin/node x.mjs")
            .expect_err("path")
            .contains("program's name"));
        assert!(parse_steps(r"C:\tools\node.exe x")
            .expect_err("path")
            .contains("program's name"));
        assert!(parse_steps("-rf x").is_err());
        assert!(parse_steps("npm\u{7}ci")
            .expect_err("bell")
            .contains("hidden"));
        assert!(parse_steps("node \"open")
            .expect_err("quote")
            .contains("closed"));
        assert!(parse_steps("  \n\n")
            .expect_err("empty")
            .contains("at least one"));
        assert!(parse_steps(&"x ".repeat(200))
            .expect_err("long")
            .contains("too long"));
        let nine = "npm ci\n".repeat(MAX_STEPS + 1);
        assert!(parse_steps(&nine)
            .expect_err("many")
            .contains("more than 8"));
        assert!(parse_steps(&"npm ci\n".repeat(MAX_STEPS)).is_ok());
        for e in [
            parse_steps("a | b").expect_err("x"),
            parse_steps("").expect_err("x"),
        ] {
            assert!(!e.contains('\u{2014}'), "no long dashes: {e}");
        }
    }

    #[test]
    fn the_live_address_is_https_with_a_plain_site_name() {
        assert_eq!(
            check_site("  https://app.pvmedispa.com  ").expect("ok"),
            "https://app.pvmedispa.com"
        );
        assert!(check_site("https://shop.example.com:8443/home?x=1").is_ok());
        assert!(check_site("http://app.pvmedispa.com").is_err());
        assert!(check_site("app.pvmedispa.com").is_err());
        assert!(check_site("https://me:pw@app.pvmedispa.com").is_err());
        assert!(check_site("https://app pvmedispa.com").is_err());
        assert!(check_site("https://").is_err());
        assert!(check_site(&format!("https://{}.com", "a".repeat(300))).is_err());
    }
}
