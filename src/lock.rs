//! Voorkomt dat twee `chronicle start`-processen tegelijk naar dezelfde
//! database schrijven. WAL-modus is gemaakt voor meerdere lezers naast één
//! schrijver, niet voor twee onafhankelijke schrijfprocessen die allebei hun
//! eigen schema-migratie en WAL-initialisatie doen bij het opstarten — precies
//! dat corrumpeerde de database tweemaal in dezelfde sessie (twee overlappende
//! `chronicle start`-instanties). Een gewoon PID-bestand zou na een crash of
//! geforceerde kill blijven liggen en elke volgende start onterecht blokkeren;
//! deze lock steunt in plaats daarvan op Windows' eigen bestandsdeling — de
//! OS-handle valt vanzelf weg zodra het proces stopt, ook bij een crash.

use anyhow::{Result, anyhow};
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

const ERROR_SHARING_VIOLATION: i32 = 32;

/// Blijft vastgehouden zolang de opname draait; laten vallen geeft de lock vrij.
pub struct InstanceLock(#[allow(dead_code)] File);

impl InstanceLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        OpenOptions::new()
            .create(true)
            .write(true)
            .share_mode(0) // geen enkele andere handle toegestaan, ook niet van onszelf
            .open(path)
            .map(Self)
            .map_err(|e| {
                if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) {
                    anyhow!("er draait al een andere opname (chronicle start) — sluit die eerst af")
                } else {
                    anyhow!("lockbestand openen mislukt: {} ({e})", path.display())
                }
            })
    }
}
