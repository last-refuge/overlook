//! The process-wide display zone. Discovery happens at startup and on a background
//! timer; formatting only clones the cached zone. Tests exercise independent states.
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZoneDefinition {
    Named(String),
    Posix(String),
    Tzif { name: String, data: Vec<u8> },
}

impl ZoneDefinition {
    pub fn resolve(&self) -> Result<TimeZone, jiff::Error> {
        match self {
            Self::Named(name) => TimeZone::get(name),
            Self::Posix(rule) => TimeZone::posix(rule),
            Self::Tzif { name, data } => TimeZone::tzif(name, data),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DetectedZone {
    definition: ZoneDefinition,
    zone: TimeZone,
}

impl DetectedZone {
    pub fn zone(&self) -> &TimeZone {
        &self.zone
    }
    pub fn definition(&self) -> &ZoneDefinition {
        &self.definition
    }
    pub fn new(definition: ZoneDefinition) -> Option<Self> {
        let zone = definition.resolve().ok()?;
        Some(Self { definition, zone })
    }
}

struct ZoneState {
    system: DetectedZone,
    manual: Option<(ZoneDefinition, TimeZone)>,
    revision: u64,
}

impl ZoneState {
    fn zone(&self) -> &TimeZone {
        self.manual
            .as_ref()
            .map(|(_, zone)| zone)
            .unwrap_or(&self.system.zone)
    }

    fn set_override(&mut self, definition: Option<&ZoneDefinition>) -> Result<bool, String> {
        let manual = definition
            .map(|definition| definition.resolve().map(|zone| (definition.clone(), zone)))
            .transpose()
            .map_err(|_| "Choose a valid time zone.".to_owned())?;
        let before = self.zone().clone();
        self.manual = manual;
        let changed = self.zone() != &before;
        self.revision += u64::from(changed);
        Ok(changed)
    }

    fn update_system(&mut self, detected: DetectedZone) -> bool {
        let before = self.zone().clone();
        self.system = detected;
        let changed = self.zone() != &before;
        self.revision += u64::from(changed);
        changed
    }

    fn cache_key(&self) -> String {
        let name = format!("{:?}", self.zone());
        let definition = self
            .manual
            .as_ref()
            .map(|(definition, _)| definition)
            .unwrap_or(&self.system.definition);
        if let ZoneDefinition::Tzif { name: source, data } = definition {
            if source == "Local" {
                // Persisted coverage/pending-edit keys must survive Rust upgrades.
                // FNV-1a is a stable file fingerprint, not a security boundary.
                let hash = data.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                });
                return format!("{name}:{hash:016x}");
            }
        }
        name
    }
}

fn state() -> &'static RwLock<ZoneState> {
    static STATE: OnceLock<RwLock<ZoneState>> = OnceLock::new();
    STATE.get_or_init(|| {
        RwLock::new(ZoneState {
            system: detect_system_zone().unwrap_or_else(|| {
                DetectedZone::new(ZoneDefinition::Named("UTC".into()))
                    .expect("UTC is always available")
            }),
            manual: None,
            revision: 0,
        })
    })
}

pub fn current_zone() -> TimeZone {
    state().read().unwrap().zone().clone()
}
pub fn system_zone() -> TimeZone {
    state().read().unwrap().system.zone.clone()
}
pub fn system_definition() -> ZoneDefinition {
    state().read().unwrap().system.definition.clone()
}
pub fn revision() -> u64 {
    state().read().unwrap().revision
}
/// Preserve existing named-zone coverage keys. Unnamed TZif files also need a
/// fingerprint: two different /etc/localtime copies can both be called "Local".
pub fn cache_key() -> String {
    state().read().unwrap().cache_key()
}
/// A coherent zone and coverage key for a calendar download.
pub fn zone_with_cache_key() -> (TimeZone, String) {
    let state = state().read().unwrap();
    (state.zone().clone(), state.cache_key())
}
pub fn current_definition() -> ZoneDefinition {
    let state = state().read().unwrap();
    match &state.manual {
        Some((definition, _)) => definition.clone(),
        None => state.system.definition.clone(),
    }
}
pub fn set_override(definition: Option<&ZoneDefinition>) -> Result<bool, String> {
    state().write().unwrap().set_override(definition)
}
pub fn update_system_zone(detected: DetectedZone) -> bool {
    state().write().unwrap().update_system(detected)
}

pub fn zone_label(zone: &TimeZone) -> String {
    if zone == &TimeZone::UTC {
        return "UTC".into();
    }
    zone.iana_name()
        .filter(|name| *name != "Local")
        .unwrap_or("Local time")
        .replace('_', " ")
}

/// This bypasses Jiff's five-minute system-zone cache. Reading the TZif data also
/// detects systems that replace /etc/localtime with a regular file, not a symlink.
pub fn detect_system_zone() -> Option<DetectedZone> {
    if let Some(value) = std::env::var_os("TZ") {
        let value = value.to_str()?.trim_start_matches(':');
        if value.is_empty() {
            return DetectedZone::new(ZoneDefinition::Named("UTC".into()));
        }
        if Path::new(value).is_absolute() {
            return read_zone_file(Path::new(value));
        }
        return DetectedZone::new(ZoneDefinition::Named(value.into()))
            .or_else(|| DetectedZone::new(ZoneDefinition::Posix(value.into())));
    }
    #[cfg(unix)]
    {
        read_zone_file(Path::new("/etc/localtime"))
    }
    #[cfg(not(unix))]
    {
        let zone = TimeZone::try_system().ok()?;
        DetectedZone::new(ZoneDefinition::Named(zone.iana_name()?.into()))
    }
}

fn read_zone_file(path: &Path) -> Option<DetectedZone> {
    if std::fs::metadata(path).ok()?.len() > 1024 * 1024 {
        return None;
    }
    let resolved = std::fs::canonicalize(path).ok()?;
    let name = resolved
        .to_str()
        .and_then(|p| p.split_once("/zoneinfo/").map(|(_, n)| n))
        .map(|n| n.trim_start_matches("posix/").to_owned())
        .unwrap_or_else(|| "Local".into());
    DetectedZone::new(ZoneDefinition::Tzif {
        name,
        data: std::fs::read(path).ok()?,
    })
}

pub fn available_zone_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<_> = jiff::tz::db()
            .available()
            .map(|name| name.to_string())
            .collect();
        names.push("UTC".into());
        names.sort();
        names.dedup();
        names
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn named(name: &str) -> DetectedZone {
        DetectedZone::new(ZoneDefinition::Named(name.into())).unwrap()
    }

    #[test]
    fn follows_system_changes_but_keeps_an_override_until_cleared() {
        let mut state = ZoneState {
            system: named("America/Toronto"),
            manual: None,
            revision: 0,
        };
        assert!(state.update_system(named("America/Phoenix")));
        assert_eq!(state.zone(), &TimeZone::get("America/Phoenix").unwrap());
        assert!(state
            .set_override(Some(&ZoneDefinition::Named("Asia/Kolkata".into())))
            .unwrap());
        let revision = state.revision;
        assert!(!state.update_system(named("Europe/Berlin")));
        assert_eq!(state.revision, revision);
        assert_eq!(state.zone(), &TimeZone::get("Asia/Kolkata").unwrap());
        assert!(state.set_override(None).unwrap());
        assert_eq!(state.zone(), &TimeZone::get("Europe/Berlin").unwrap());
        assert!(!state.update_system(named("Europe/Berlin")));
    }

    #[test]
    fn an_invalid_override_preserves_the_active_zone() {
        let mut state = ZoneState {
            system: named("UTC"),
            manual: None,
            revision: 0,
        };
        assert!(state
            .set_override(Some(&ZoneDefinition::Named("Not/AZone".into())))
            .is_err());
        assert_eq!(state.revision, 0);
        assert_eq!(state.zone(), &TimeZone::get("UTC").unwrap());
    }

    #[test]
    fn different_zones_with_the_same_current_offset_still_refresh() {
        let mut state = ZoneState {
            system: named("America/Toronto"),
            manual: None,
            revision: 0,
        };
        assert!(state.update_system(named("America/New_York")));
        assert_eq!(state.revision, 1);
        assert!(available_zone_names()
            .iter()
            .any(|name| name == "America/Toronto"));
    }

    #[cfg(unix)]
    #[test]
    fn detects_replaced_localtime_files_without_waiting_for_a_cache() {
        let folder =
            std::env::temp_dir().join(format!("overlook-zone-{}", crate::DraftId::generate()));
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("localtime");
        std::fs::copy("/usr/share/zoneinfo/America/Toronto", &file).unwrap();
        let first = read_zone_file(&file).unwrap();
        std::fs::copy("/usr/share/zoneinfo/America/Phoenix", &file).unwrap();
        let second = read_zone_file(&file).unwrap();
        assert_ne!(first.zone, second.zone);
        assert_eq!(second.definition.resolve().unwrap(), second.zone);
        let mut state = ZoneState {
            system: first.clone(),
            manual: None,
            revision: 0,
        };
        let first_key = state.cache_key();
        // Keeping the old system zone works even when /etc/localtime was a copy,
        // with no IANA name. The serialized override survives restart.
        let kept: ZoneDefinition =
            serde_json::from_str(&serde_json::to_string(&first.definition).unwrap()).unwrap();
        state.set_override(Some(&kept)).unwrap();
        assert!(!state.update_system(second));
        assert_eq!(state.zone(), first.zone());
        assert_eq!(state.cache_key(), first_key);
        assert!(state.set_override(None).unwrap());
        assert_ne!(state.cache_key(), first_key);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
