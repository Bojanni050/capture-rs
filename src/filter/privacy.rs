//! Privacy: apps die we nooit vastleggen, en patronen die we wegstrepen.
//!
//! Een tool die alles onthoudt, moet ook kunnen vergeten. Twee mechanismen:
//! hele vensters overslaan (wachtwoordkluizen, privémodus) en gevoelige
//! patronen redigeren voordat er iets naar de database gaat.

use anyhow::{Context, Result};
use regex::Regex;

/// Patronen die standaard geredigeerd worden. Bewust conservatief: liever
/// een kaartnummer laten staan dan je hele broncode onleesbaar maken.
const DEFAULT_PATTERNS: &[&str] = &[
    // Creditcardnummers in de gebruikelijke groepering van vier.
    r"\b\d{4}[ -]?\d{4}[ -]?\d{4}[ -]?\d{4}\b",
    // IBAN, inclusief de Nederlandse vorm NL91 ABNA 0417 1643 00.
    r"\b[A-Z]{2}\d{2} ?(?:[A-Z0-9]{4} ?){2,7}[A-Z0-9]{1,4}\b",
    // Sleutel/waarde-paren waarvan de sleutel het al verraadt.
    r"(?i)\b(?:password|wachtwoord|passwd|secret|api[_-]?key|access[_-]?token|bearer)\b\s*[:=]\s*\S+",
    // Tokens met een herkenbaar voorvoegsel (GitHub, Slack, Stripe).
    r"\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{10,}\b",
    r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
    r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
    // Privésleutels: alles vanaf de header is per definitie geheim.
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
    // Burgerservicenummer, alleen als het label erbij staat.
    r"(?i)\bbsn\b\D{0,10}\d{9}\b",
];

pub struct Redactor {
    patterns: Vec<Regex>,
    enabled: bool,
}

impl Redactor {
    pub fn new(enabled: bool, extra: &[String]) -> Result<Self> {
        let mut patterns = Vec::new();
        if enabled {
            for p in DEFAULT_PATTERNS {
                patterns.push(Regex::new(p).expect("ingebouwd patroon is geldig"));
            }
            for p in extra {
                patterns.push(
                    Regex::new(p).with_context(|| format!("ongeldige redact_extra-regex: {p}"))?,
                );
            }
        }
        Ok(Self { patterns, enabled })
    }

    /// Vervangt elk gevonden patroon door `[REDACTED]`.
    pub fn apply(&self, text: &str) -> String {
        if !self.enabled || self.patterns.is_empty() {
            return text.to_string();
        }
        let mut out = text.to_string();
        for re in &self.patterns {
            out = re.replace_all(&out, "[REDACTED]").into_owned();
        }
        out
    }
}

/// Beslist of een venster überhaupt vastgelegd mag worden.
pub struct Denylist {
    apps: Vec<String>,
    titles: Vec<Regex>,
}

impl Denylist {
    pub fn new(apps: &[String], titles: &[String]) -> Result<Self> {
        let mut compiled = Vec::new();
        for t in titles {
            compiled
                .push(Regex::new(t).with_context(|| format!("ongeldige title_denylist-regex: {t}"))?);
        }
        Ok(Self {
            apps: apps.iter().map(|a| a.to_lowercase()).collect(),
            titles: compiled,
        })
    }

    /// `app_key` is de procesnaam zonder `.exe`, in kleine letters.
    pub fn blocks_app(&self, app_key: &str) -> bool {
        let key = app_key.to_lowercase();
        self.apps.iter().any(|a| key.contains(a.as_str()))
    }

    pub fn blocks_title(&self, title: &str) -> bool {
        self.titles.iter().any(|re| re.is_match(title))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(true, &[]).unwrap()
    }

    #[test]
    fn creditcard_wordt_geredigeerd() {
        let out = redactor().apply("betaald met 4111 1111 1111 1111 vandaag");
        assert!(!out.contains("4111"), "{out}");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn iban_wordt_geredigeerd() {
        let out = redactor().apply("overmaken naar NL91 ABNA 0417 1643 00 graag");
        assert!(!out.contains("ABNA"), "{out}");
    }

    #[test]
    fn wachtwoordveld_wordt_geredigeerd() {
        let out = redactor().apply("password: hunter2ismijngeheim");
        assert!(!out.contains("hunter2"), "{out}");
    }

    #[test]
    fn gewone_tekst_blijft_intact() {
        let text = "De build draaide in 42 seconden op branch main.";
        assert_eq!(redactor().apply(text), text);
    }

    #[test]
    fn uitgeschakeld_laat_alles_staan() {
        let r = Redactor::new(false, &[]).unwrap();
        assert_eq!(r.apply("password: geheim"), "password: geheim");
    }

    #[test]
    fn denylist_matcht_op_deel_van_de_naam() {
        let d = Denylist::new(&["bitwarden".into()], &[r"(?i)incognito".into()]).unwrap();
        assert!(d.blocks_app("Bitwarden"));
        assert!(!d.blocks_app("notepad"));
        assert!(d.blocks_title("Nieuw tabblad - Incognito"));
        assert!(!d.blocks_title("Nieuw tabblad"));
    }
}
