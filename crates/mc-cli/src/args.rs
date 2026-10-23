//! Small argument parser: `--k v`, `--k=v`, boolean flags, `--` and positionals.

use std::collections::{BTreeMap, BTreeSet};

use crate::cli::CliError;

/// Options a command accepts (the global ones are always accepted).
pub struct Spec {
    pub values: &'static [&'static str],
    pub flags: &'static [&'static str],
}

const GLOBAL_VALUES: &[&str] = &["config", "data-dir"];
const GLOBAL_FLAGS: &[&str] = &["json", "help"];

#[derive(Debug, Default)]
pub struct Parsed {
    pub values: BTreeMap<String, String>,
    pub flags: BTreeSet<String>,
    pub positional: Vec<String>,
}

impl Parsed {
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
}

pub fn parse(args: &[String], spec: &Spec) -> Result<Parsed, CliError> {
    let is_value = |name: &str| spec.values.contains(&name) || GLOBAL_VALUES.contains(&name);
    let is_flag = |name: &str| spec.flags.contains(&name) || GLOBAL_FLAGS.contains(&name);
    let mut parsed = Parsed::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            parsed.positional.extend(iter.by_ref().cloned());
            break;
        }
        if arg == "-h" {
            parsed.flags.insert("help".into());
            continue;
        }
        let Some(option) = arg.strip_prefix("--") else {
            if arg.starts_with('-') && arg.len() > 1 {
                return Err(CliError::usage(format!("unknown option {arg}")));
            }
            parsed.positional.push(arg.clone());
            continue;
        };
        let (name, inline) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (option, None),
        };
        if is_value(name) {
            let value = match inline {
                Some(value) => value,
                None => iter
                    .next()
                    .cloned()
                    .ok_or_else(|| CliError::usage(format!("--{name} needs a value")))?,
            };
            parsed.values.insert(name.to_string(), value);
        } else if is_flag(name) {
            if inline.is_some() {
                return Err(CliError::usage(format!("--{name} takes no value")));
            }
            parsed.flags.insert(name.to_string());
        } else {
            return Err(CliError::usage(format!("unknown option --{name}")));
        }
    }
    Ok(parsed)
}
