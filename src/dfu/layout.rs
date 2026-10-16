//! DfuSe memory-layout strings — the alt-setting names an STM32 ROM
//! bootloader advertises, e.g. `@Internal Flash  /0x08000000/8*128Kg`
//! (AN3156 §4.1, the "DfuSe" interface-string format).
//!
//! Grammar: `@<name>/<start>/<count>*<size><unit><flags>[,<count>*…]…`
//! where `unit` is ` ` (bytes), `K` or `M`, and `flags` is one letter
//! `a`..`g` encoding readable / erasable / writable bits.

/// One run of equal-size sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectorRun {
    pub count: u32,
    pub size: u32,
    /// The DfuSe flag letter (`a`..`g`): bit 0 readable, bit 1
    /// erasable, bit 2 writable.
    pub flags: u8,
}

/// A parsed memory region (the part we care about is "Internal Flash").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLayout {
    pub name: String,
    pub start: u32,
    pub runs: Vec<SectorRun>,
}

impl MemoryLayout {
    /// Total size in bytes.
    pub fn size(&self) -> u64 {
        self.runs
            .iter()
            .map(|r| u64::from(r.count) * u64::from(r.size))
            .sum()
    }

    /// Whether `[addr, addr + len)` lies inside this region.
    pub fn contains(&self, addr: u64, len: u64) -> bool {
        let start = u64::from(self.start);
        addr >= start && addr.saturating_add(len) <= start + self.size()
    }

    /// The single-bank 1 MB STM32H72x/73x layout: 8 × 128 KiB sectors at
    /// 0x08000000. Nothing else in the H7 family looks like this (the
    /// H74x/75x have two banks / 16 sectors, the H7A3/B0 8 KiB sectors,
    /// the H730/H750 a single 128 KiB sector), so it doubles as the chip
    /// check for provisioning — the seed address only exists here.
    pub fn is_h72x_73x_1mb(&self) -> bool {
        self.start == 0x0800_0000
            && self.runs.len() == 1
            && self.runs[0].count == 8
            && self.runs[0].size == 128 * 1024
    }
}

/// Parse a DfuSe layout string. `None` if it isn't one.
pub fn parse(raw: &str) -> Option<MemoryLayout> {
    let s = raw.trim().strip_prefix('@')?;
    let mut parts = s.split('/');
    let name = parts.next()?.trim().to_string();
    let start_raw = parts.next()?.trim();
    let start = u32::from_str_radix(
        start_raw
            .strip_prefix("0x")
            .or_else(|| start_raw.strip_prefix("0X"))?,
        16,
    )
    .ok()?;
    let runs_raw = parts.next()?;
    let mut runs = Vec::new();
    for item in runs_raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (count, rest) = item.split_once('*')?;
        let count: u32 = count.trim().parse().ok()?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let mut tail = rest[digits.len()..].chars();
        let base: u32 = digits.parse().ok()?;
        let (mult, flags) = match (tail.next(), tail.next()) {
            (Some('K'), Some(f)) => (1024, f),
            (Some('M'), Some(f)) => (1024 * 1024, f),
            (Some(' '), Some(f)) => (1, f),
            (Some(f), None) if f.is_ascii_lowercase() => (1, f),
            _ => return None,
        };
        if !('a'..='g').contains(&flags) || count == 0 {
            return None;
        }
        runs.push(SectorRun {
            count,
            size: base.checked_mul(mult)?,
            flags: flags as u8 - b'a' + 1,
        });
    }
    if runs.is_empty() {
        return None;
    }
    Some(MemoryLayout { name, start, runs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_h7_internal_flash_layout() {
        let l = parse("@Internal Flash  /0x08000000/8*128Kg").unwrap();
        assert_eq!(l.name, "Internal Flash");
        assert_eq!(l.start, 0x0800_0000);
        assert_eq!(l.size(), 1024 * 1024);
        assert_eq!(l.runs[0].flags, 7, "g = readable + erasable + writable");
        assert!(l.is_h72x_73x_1mb());
        assert!(l.contains(0x080F_FFC0, 32));
        assert!(!l.contains(0x080F_FFF0, 32), "runs past the end");
    }

    #[test]
    fn parses_mixed_runs_and_byte_units() {
        let l = parse("@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg").unwrap();
        assert_eq!(l.runs.len(), 3);
        assert_eq!(l.size(), (4 * 16 + 64 + 7 * 128) * 1024);
        assert!(!l.is_h72x_73x_1mb());
        let ob = parse("@Option Bytes  /0x5200201C/01*128 e").unwrap();
        assert_eq!(ob.runs[0].size, 128);
    }

    #[test]
    fn rejects_non_layout_strings() {
        for bad in [
            "Internal Flash",
            "@x/08000000/8*128Kg",
            "@x/0x08000000/8*128Kz",
            "@x/0x08000000/",
            "",
        ] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
        // Two banks / 16 sectors is NOT the H72x/73x part.
        assert!(!parse("@Internal Flash /0x08000000/16*128Kg")
            .unwrap()
            .is_h72x_73x_1mb());
    }
}
