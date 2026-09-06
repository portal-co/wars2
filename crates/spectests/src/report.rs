//! Reporting structures and output for the spectests harness.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Assertion {
    Module,
    Action,
    Return,
    Trap,
    Exhaustion,
    Invalid,
    Malformed,
    Unlinkable,
    Other,
}

impl Assertion {
    pub fn name(self) -> &'static str {
        match self {
            Assertion::Module => "module",
            Assertion::Action => "action",
            Assertion::Return => "assert_return",
            Assertion::Trap => "assert_trap",
            Assertion::Exhaustion => "assert_exhaustion",
            Assertion::Invalid => "assert_invalid",
            Assertion::Malformed => "assert_malformed",
            Assertion::Unlinkable => "assert_unlinkable",
            Assertion::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Outcome {
    Pass,
    Fail { msg: String },
    Skip { msg: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseResult {
    pub index: usize,
    pub line: u64,
    pub assertion: Assertion,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileResult {
    pub file: String,
    pub backend: String,
    pub cases: Vec<CaseResult>,
}

impl FileResult {
    pub fn new(file: String, backend: crate::runner::Backend) -> Self {
        Self {
            file,
            backend: backend.name().to_string(),
            cases: vec![],
        }
    }
    pub fn push(&mut self, c: CaseResult) {
        self.cases.push(c);
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for case in &self.cases {
            match &case.outcome {
                Outcome::Pass => c.pass += 1,
                Outcome::Fail { .. } => c.fail += 1,
                Outcome::Skip { .. } => c.skip += 1,
            }
        }
        c
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    pub pass: usize,
    pub fail: usize,
    pub skip: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub files: Vec<FileResult>,
}

impl Report {
    pub fn totals(&self) -> Counts {
        let mut t = Counts::default();
        for f in &self.files {
            let c = f.counts();
            t.pass += c.pass;
            t.fail += c.fail;
            t.skip += c.skip;
        }
        t
    }

    /// Render a markdown summary (for CI job summaries).
    pub fn summary_markdown(&self) -> String {
        let mut out = String::from("| file | backend | pass | fail | skip |\n|---|---|---|---|---|\n");
        for f in &self.files {
            let c = f.counts();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                f.file, f.backend, c.pass, c.fail, c.skip
            ));
        }
        let t = self.totals();
        out.push_str(&format!(
            "\n**Total:** {} pass, {} fail, {} skip\n",
            t.pass, t.fail, t.skip
        ));
        out
    }

    /// Write the report as JSON.
    pub fn write_json(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Per-file failure details, for debugging.
    pub fn failures(&self) -> BTreeMap<String, Vec<(usize, u64, &str, &str)>> {
        let mut out = BTreeMap::new();
        for f in &self.files {
            let mut v = vec![];
            for c in &f.cases {
                if let Outcome::Fail { msg } = &c.outcome {
                    v.push((c.index, c.line, c.assertion.name(), msg.as_str()));
                }
            }
            if !v.is_empty() {
                out.insert(format!("{} [{}]", f.file, f.backend), v);
            }
        }
        out
    }
}
