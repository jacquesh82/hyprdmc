//! Dialogue avec Hyprland par ses deux sockets UNIX, sans passer par `hyprctl`.
//!
//! * `.socket.sock`  — requêtes/commandes. Une connexion par commande : on
//!   écrit la commande, on lit la réponse jusqu'à EOF.
//! * `.socket2.sock` — flux d'événements. Connexion longue durée, lignes de la
//!   forme `EVENEMENT>>DONNEES\n`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::monitor::Monitor;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Abstraction du transport vers Hyprland.
///
/// Tout le reste du programme ne dépend que de ce trait, ce qui permet de
/// tester la logique métier sans compositeur en fonctionnement.
pub trait HyprBackend: Send + Sync {
    /// Envoie une commande brute et retourne la réponse textuelle.
    fn query(&self, cmd: &str) -> Result<String>;

    /// Applique plusieurs commandes en une seule transaction Hyprland.
    fn batch(&self, cmds: &[String]) -> Result<String> {
        if cmds.is_empty() {
            return Ok(String::new());
        }
        self.query(&format!("[[BATCH]]{}", cmds.join(";")))
    }

    /// État de tous les écrans, y compris ceux qui sont désactivés.
    fn monitors(&self) -> Result<Vec<Monitor>> {
        let raw = self.query("j/monitors all")?;
        serde_json::from_str(&raw)
            .with_context(|| format!("réponse JSON inattendue de Hyprland : {}", truncate(&raw)))
    }

    /// Applique une série de directives `monitor = …` et vérifie qu'aucune n'a
    /// été rejetée.
    fn set_monitors(&self, specs: &[String]) -> Result<()> {
        let cmds: Vec<String> = specs
            .iter()
            .map(|s| format!("keyword monitor {s}"))
            .collect();
        let reply = self.batch(&cmds)?;
        check_ok(&reply, &cmds)
    }
}

/// Hyprland répond `ok` par commande acceptée ; tout le reste est un message
/// d'erreur qu'il faut remonter tel quel à l'utilisateur.
fn check_ok(reply: &str, cmds: &[String]) -> Result<()> {
    let trimmed = reply.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c.is_whitespace()) {
        return Ok(());
    }
    // La réponse d'un batch est la concaténation des « ok ».
    let leftovers = trimmed.replace("ok", "");
    if leftovers.trim().is_empty() {
        return Ok(());
    }
    bail!(
        "Hyprland a rejeté la configuration : {}\ncommandes envoyées :\n  {}",
        trimmed,
        cmds.join("\n  ")
    );
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 200 {
        s.to_string()
    } else {
        format!("{}…", &s[..200])
    }
}

/// Localise le répertoire de l'instance Hyprland courante.
pub fn instance_dir() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", unsafe { libc_getuid() })));
    let base = runtime.join("hypr");

    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let dir = base.join(&sig);
        if dir.is_dir() {
            return Ok(dir);
        }
        bail!(
            "HYPRLAND_INSTANCE_SIGNATURE vaut « {sig} » mais {} n'existe pas",
            dir.display()
        );
    }

    // Hors session Hyprland (systemd --user par exemple) : s'il n'y a qu'une
    // seule instance, on la prend.
    let mut instances: Vec<PathBuf> = std::fs::read_dir(&base)
        .with_context(|| {
            format!(
                "Hyprland ne semble pas tourner : {} introuvable",
                base.display()
            )
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(".socket.sock").exists())
        .collect();
    instances.sort();

    match instances.len() {
        0 => bail!("aucune instance Hyprland trouvée dans {}", base.display()),
        1 => Ok(instances.remove(0)),
        n => bail!(
            "{n} instances Hyprland trouvées dans {} — définissez HYPRLAND_INSTANCE_SIGNATURE",
            base.display()
        ),
    }
}

// Évite une dépendance à la crate `libc` pour un unique appel.
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Transport réel : les sockets UNIX de Hyprland.
#[derive(Debug, Clone)]
pub struct HyprSocket {
    dir: PathBuf,
}

impl HyprSocket {
    pub fn connect() -> Result<Self> {
        Ok(Self {
            dir: instance_dir()?,
        })
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn request_socket(&self) -> PathBuf {
        self.dir.join(".socket.sock")
    }

    pub fn event_socket(&self) -> PathBuf {
        self.dir.join(".socket2.sock")
    }
}

impl HyprBackend for HyprSocket {
    fn query(&self, cmd: &str) -> Result<String> {
        let path = self.request_socket();
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("connexion à {} impossible", path.display()))?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        stream
            .write_all(cmd.as_bytes())
            .with_context(|| format!("envoi de « {cmd} » impossible"))?;
        stream.flush()?;

        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .with_context(|| format!("lecture de la réponse à « {cmd} » impossible"))?;
        String::from_utf8(buf).map_err(|e| anyhow!("réponse non-UTF8 de Hyprland : {e}"))
    }
}

/// Événements du socket 2 qui nous concernent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprEvent {
    /// Un écran est apparu (nom du connecteur).
    MonitorAdded(String),
    /// Un écran a disparu (nom du connecteur).
    MonitorRemoved(String),
    /// La configuration a été rechargée : l'état a pu changer sous nos pieds.
    ConfigReloaded,
    /// Tout le reste, conservé pour le journal en mode verbeux.
    Other(String),
}

impl HyprEvent {
    /// Analyse une ligne `EVENEMENT>>DONNEES`.
    pub fn parse(line: &str) -> Option<Self> {
        let (event, data) = line.split_once(">>")?;
        Some(match event {
            // `monitoraddedv2` porte « ID,NOM,DESCRIPTION » ; on ne garde que le nom.
            "monitoraddedv2" => {
                let mut parts = data.splitn(3, ',');
                let _id = parts.next();
                let name = parts.next().unwrap_or_default().to_string();
                HyprEvent::MonitorAdded(name)
            }
            "monitoradded" => HyprEvent::MonitorAdded(data.to_string()),
            "monitorremoved" | "monitorremovedv2" => {
                // La variante v2 porte « ID,NOM,DESCRIPTION ».
                let name = if event.ends_with("v2") {
                    data.split(',').nth(1).unwrap_or_default().to_string()
                } else {
                    data.to_string()
                };
                HyprEvent::MonitorRemoved(name)
            }
            "configreloaded" => HyprEvent::ConfigReloaded,
            _ => HyprEvent::Other(line.to_string()),
        })
    }

    /// Un événement qui doit déclencher une réévaluation du profil.
    pub fn affects_monitors(&self) -> bool {
        matches!(
            self,
            HyprEvent::MonitorAdded(_) | HyprEvent::MonitorRemoved(_)
        )
    }
}

/// Ouvre le flux d'événements et invoque `on_event` pour chaque ligne.
///
/// La fonction ne rend la main que si le socket se ferme (Hyprland qui
/// redémarre) ou si `on_event` retourne une erreur.
pub async fn stream_events<F>(socket: &Path, mut on_event: F) -> Result<()>
where
    F: FnMut(HyprEvent) -> Result<()>,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixStream as AsyncUnixStream;

    let stream = AsyncUnixStream::connect(socket).await.with_context(|| {
        format!(
            "connexion au flux d'événements {} impossible",
            socket.display()
        )
    })?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(event) = HyprEvent::parse(&line) {
            on_event(event)?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod fake {
    //! Backend de test : rejoue des réponses figées et enregistre les commandes.

    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeBackend {
        pub monitors_json: Mutex<String>,
        pub sent: Mutex<Vec<String>>,
        pub fail_with: Mutex<Option<String>>,
        /// Réponses successives à `j/monitors`, pour simuler un état qui met
        /// quelques relectures à converger. La dernière reste servie ensuite.
        pending_states: Mutex<Vec<String>>,
    }

    impl FakeBackend {
        pub fn with_monitors(json: &str) -> Self {
            Self {
                monitors_json: Mutex::new(json.to_string()),
                ..Default::default()
            }
        }

        /// Backend qui rend `stale` pendant `repeats` relectures avant de
        /// rendre `settled` — reproduit la latence d'application de Hyprland.
        pub fn settling_after(repeats: usize, stale: &str, settled: &str) -> Self {
            let mut states = vec![stale.to_string(); repeats];
            states.reverse();
            Self {
                monitors_json: Mutex::new(settled.to_string()),
                pending_states: Mutex::new(states),
                ..Default::default()
            }
        }

        pub fn sent_commands(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }

        pub fn monitor_reads(&self) -> usize {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("j/monitors"))
                .count()
        }
    }

    impl HyprBackend for FakeBackend {
        fn query(&self, cmd: &str) -> Result<String> {
            self.sent.lock().unwrap().push(cmd.to_string());
            if let Some(err) = self.fail_with.lock().unwrap().clone() {
                return Ok(err);
            }
            if cmd.starts_with("j/monitors") {
                if let Some(stale) = self.pending_states.lock().unwrap().pop() {
                    return Ok(stale);
                }
                return Ok(self.monitors_json.lock().unwrap().clone());
            }
            Ok("ok".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_monitor_added_v2() {
        assert_eq!(
            HyprEvent::parse("monitoraddedv2>>1,DP-1,Dell Inc. U2723QE ABC"),
            Some(HyprEvent::MonitorAdded("DP-1".into()))
        );
    }

    #[test]
    fn parses_monitor_added_v1() {
        assert_eq!(
            HyprEvent::parse("monitoradded>>DP-1"),
            Some(HyprEvent::MonitorAdded("DP-1".into()))
        );
    }

    #[test]
    fn parses_monitor_removed_both_variants() {
        assert_eq!(
            HyprEvent::parse("monitorremoved>>DP-1"),
            Some(HyprEvent::MonitorRemoved("DP-1".into()))
        );
        assert_eq!(
            HyprEvent::parse("monitorremovedv2>>1,DP-1,Dell"),
            Some(HyprEvent::MonitorRemoved("DP-1".into()))
        );
    }

    #[test]
    fn unrelated_events_are_ignored_by_the_daemon() {
        let ev = HyprEvent::parse("workspace>>2").unwrap();
        assert!(!ev.affects_monitors());
        assert!(
            HyprEvent::parse("monitoradded>>DP-1")
                .unwrap()
                .affects_monitors()
        );
    }

    #[test]
    fn malformed_lines_yield_nothing() {
        assert_eq!(HyprEvent::parse("pas de séparateur"), None);
    }

    #[test]
    fn batch_joins_commands() {
        let fake = fake::FakeBackend::default();
        fake.batch(&["keyword monitor A".into(), "keyword monitor B".into()])
            .unwrap();
        assert_eq!(
            fake.sent_commands(),
            vec!["[[BATCH]]keyword monitor A;keyword monitor B"]
        );
    }

    #[test]
    fn empty_batch_is_a_no_op() {
        let fake = fake::FakeBackend::default();
        fake.batch(&[]).unwrap();
        assert!(fake.sent_commands().is_empty());
    }

    #[test]
    fn rejected_configuration_surfaces_hyprland_message() {
        let cmds = vec!["keyword monitor DP-1,bogus".to_string()];
        let err = check_ok("Invalid mode for monitor", &cmds).unwrap_err();
        assert!(err.to_string().contains("Invalid mode"));
        assert!(check_ok("okok", &cmds).is_ok());
    }
}
