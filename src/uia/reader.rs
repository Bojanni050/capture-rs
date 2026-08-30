//! Tekst lezen uit de accessibility-boom via UI Automation.
//!
//! Waar OCR pixels moet raden, geeft UIA de échte tekens die de app aan
//! schermlezers doorgeeft: exact, en veel goedkoper dan een OCR-pass.
//!
//! De valkuil van UIA is het aantal COM-aanroepen. Naïef door de boom lopen
//! kost één cross-process call per knoop per property — op een browserpagina
//! zijn dat er tienduizenden, en dan ben je trager dan OCR. Daarom halen we de
//! hele deelboom in **één** `FindAllBuildCache` op, met vooraf gedeclareerde
//! properties, en lezen we daarna alleen nog uit de cache.

// De UIA-constanten heten in de Windows-headers `UIA_TextControlTypeId` en niet
// `UIA_TEXT_CONTROL_TYPE_ID`; ze hernoemen zou ze onvindbaar maken in de
// Microsoft-documentatie.
#![allow(non_upper_case_globals)]

use crate::com::ensure_mta;
use anyhow::{anyhow, Context, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::{
    IUIAutomationCondition,
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest,
    IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationValuePattern,
    TreeScope_Subtree, UIA_ControlTypePropertyId, UIA_CONTROLTYPE_ID,
    UIA_DataItemControlTypeId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    UIA_HyperlinkControlTypeId, UIA_IsOffscreenPropertyId, UIA_ListItemControlTypeId,
    UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_TabItemControlTypeId,
    UIA_TextControlTypeId, UIA_TextPatternId, UIA_TreeItemControlTypeId, UIA_ValuePatternId,
};

/// Boven deze lengte is een losse Name geen label meer maar een heel document;
/// die knippen we op regels zodat de rest van de pijplijn er normaal mee omgaat.
const LONG_TEXT_SPLIT: usize = 200;
/// Namen langer dan dit kappen we af — sommige apps stoppen hun hele DOM erin.
const MAX_SINGLE_TEXT: usize = 20_000;

pub struct UiaReader {
    pub(crate) automation: IUIAutomation,
    pub(crate) cache: IUIAutomationCacheRequest,
    /// De content view: alleen knopen die inhoud dragen. De raw view bevat
    /// daarnaast elk decoratief paneel en elke scrollbar, en op een
    /// Electron-venster scheelt dat een orde van grootte in tijd.
    content_view: IUIAutomationCondition,
    max_elements: i32,
}

/// Wat één venster opleverde, inclusief de cijfers waarmee `doctor` kan zeggen
/// *waarom* er niets uitkwam: een lege boom is iets anders dan een boom vol
/// knoppen zonder tekst.
#[derive(Debug, Clone, Default)]
pub struct WindowRead {
    pub lines: Vec<String>,
    /// Aantal knopen in de deelboom, vóór enige filtering.
    pub elements: i32,
    /// Leverde het TextPattern van het venster zelf inhoud op?
    pub document_text: bool,
    /// Tijd in de TextPattern-fase.
    pub document_ms: u128,
    /// Tijd in de boomwandeling.
    pub tree_ms: u128,
}

impl WindowRead {
    pub fn chars(&self) -> usize {
        self.lines.iter().map(|l| l.chars().count()).sum()
    }
}

impl UiaReader {
    pub fn new(max_elements: usize) -> Result<Self> {
        ensure_mta()?;

        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .context("UI Automation is niet beschikbaar")?;

        let cache = unsafe { automation.CreateCacheRequest() }
            .context("UIA-cacheverzoek aanmaken mislukt")?;

        let content_view = unsafe { automation.ContentViewCondition() }
            .context("content view opvragen mislukt")?;

        unsafe {
            // Precies de properties die we straks lezen, en niets meer.
            cache.AddProperty(UIA_NamePropertyId)?;
            cache.AddProperty(UIA_ControlTypePropertyId)?;
            cache.AddProperty(UIA_IsOffscreenPropertyId)?;
            cache.AddPattern(UIA_ValuePatternId)?;
            cache.SetTreeScope(TreeScope_Subtree)?;
            cache.SetTreeFilter(&content_view)?;
            // None-modus levert cache-only elementen op: lichter voor ons én
            // voor de app aan de andere kant, want er blijft geen live
            // referentie hangen.
            cache.SetAutomationElementMode(AutomationElementMode_None)?;
        }

        Ok(Self {
            automation,
            cache,
            content_view,
            max_elements: max_elements.clamp(50, 20_000) as i32,
        })
    }

    /// Leest alle zichtbare tekst uit één venster, in boomvolgorde.
    pub fn read_window(&self, hwnd: HWND) -> Result<WindowRead> {
        if hwnd.is_invalid() {
            return Err(anyhow!("ongeldig vensterhandvat"));
        }

        let root = unsafe { self.automation.ElementFromHandle(hwnd) }
            .context("venster heeft geen accessibility-element")?;

        let mut lines = Vec::new();

        // Documenten en tekstvelden geven via TextPattern hun volledige inhoud
        // in één keer, inclusief wat buiten beeld staat. Dat is de rijkste
        // bron, dus die proberen we eerst.
        let started = std::time::Instant::now();
        let document_text = match self.document_text(&root) {
            Some(text) => {
                push_text(&mut lines, &text);
                true
            }
            None => false,
        };
        let document_ms = started.elapsed().as_millis();

        // Daarna de brede sweep: labels, knoppen, links, lijstitems.
        let started = std::time::Instant::now();
        let found =
            unsafe { root.FindAllBuildCache(TreeScope_Subtree, &self.content_view, &self.cache) }
                .context("accessibility-boom uitlezen mislukt")?;

        let elements = unsafe { found.Length() }.unwrap_or(0);
        for i in 0..elements.min(self.max_elements) {
            let Ok(element) = (unsafe { found.GetElement(i) }) else {
                continue;
            };
            self.collect(&element, &mut lines);
        }
        let tree_ms = started.elapsed().as_millis();

        Ok(WindowRead {
            lines,
            elements,
            document_text,
            document_ms,
            tree_ms,
        })
    }

    /// Tekst uit het TextPattern van het element zelf, als het dat aanbiedt.
    fn document_text(&self, element: &IUIAutomationElement) -> Option<String> {
        let pattern: IUIAutomationTextPattern =
            unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }.ok()?;
        let range = unsafe { pattern.DocumentRange() }.ok()?;
        let text = unsafe { range.GetText(MAX_SINGLE_TEXT as i32) }.ok()?;
        let text = text.to_string();
        (!text.trim().is_empty()).then_some(text)
    }

    /// Haalt de tekst uit één gecachet element.
    fn collect(&self, element: &IUIAutomationElement, out: &mut Vec<String>) {
        // Wat buiten beeld staat hoort niet bij "wat je nu ziet".
        if unsafe { element.CachedIsOffscreen() }
            .map(|b| b.as_bool())
            .unwrap_or(false)
        {
            return;
        }

        let control_type = unsafe { element.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));

        if let Ok(name) = unsafe { element.CachedName() } {
            let name = name.to_string();
            if carries_text(control_type, &name) {
                push_text(out, &name);
            }
        }

        // Invoervelden dragen hun inhoud in de Value, niet in de Name: de URL
        // in je adresbalk, de tekst in een zoekveld.
        if matches!(control_type, UIA_EditControlTypeId | UIA_DocumentControlTypeId)
            && let Ok(pattern) = unsafe {
                element.GetCachedPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            }
            && let Ok(value) = unsafe { pattern.CachedValue() }
        {
            push_text(out, &value.to_string());
        }
    }
}

/// Draagt dit element betekenisvolle tekst, of is het chrome?
///
/// Containers (panelen, groepen, vensters) hebben vaak een Name die alleen de
/// venstertitel herhaalt; die hebben we al. Alleen elementen die daadwerkelijk
/// tekst tonen tellen mee.
fn carries_text(control_type: UIA_CONTROLTYPE_ID, name: &str) -> bool {
    if name.trim().is_empty() {
        return false;
    }
    matches!(
        control_type,
        UIA_TextControlTypeId
            | UIA_EditControlTypeId
            | UIA_DocumentControlTypeId
            | UIA_HyperlinkControlTypeId
            | UIA_ListItemControlTypeId
            | UIA_TreeItemControlTypeId
            | UIA_DataItemControlTypeId
            | UIA_TabItemControlTypeId
            | UIA_MenuItemControlTypeId
    )
}

/// Voegt tekst toe, opgeknipt op regels als het een heel blok is.
fn push_text(out: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }

    if text.len() > LONG_TEXT_SPLIT || text.contains('\n') {
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.push(truncate_chars(line, MAX_SINGLE_TEXT));
            }
        }
    } else {
        out.push(text.to_string());
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tekens_tellen_over_alle_regels() {
        let read = WindowRead {
            lines: vec!["café".into(), "de".into()],
            ..Default::default()
        };
        // Tellen in tekens, niet in bytes.
        assert_eq!(read.chars(), 6);
        assert_eq!(WindowRead::default().chars(), 0);
    }

    #[test]
    fn lege_namen_tellen_niet_mee() {
        assert!(!carries_text(UIA_TextControlTypeId, "   "));
        assert!(carries_text(UIA_TextControlTypeId, "Factuurnummer"));
    }

    #[test]
    fn containers_dragen_geen_tekst() {
        // 50033 is UIA_PaneControlTypeId: een container, geen inhoud.
        assert!(!carries_text(UIA_CONTROLTYPE_ID(50033), "Hoofdvenster"));
        assert!(carries_text(UIA_HyperlinkControlTypeId, "Lees verder"));
    }

    #[test]
    fn lange_tekst_wordt_op_regels_gesplitst() {
        let mut out = Vec::new();
        push_text(&mut out, "eerste regel\n\ntweede regel\n  derde  ");
        assert_eq!(out, vec!["eerste regel", "tweede regel", "derde"]);
    }

    #[test]
    fn korte_tekst_blijft_een_geheel() {
        let mut out = Vec::new();
        push_text(&mut out, "  Opslaan  ");
        assert_eq!(out, vec!["Opslaan"]);
    }

    #[test]
    fn lege_tekst_levert_niets_op() {
        let mut out = Vec::new();
        push_text(&mut out, "   \n  \n ");
        assert!(out.is_empty());
    }
}
