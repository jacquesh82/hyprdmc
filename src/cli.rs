//! Définition de la ligne de commande.

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "hyprmc",
    version,
    about = "Gestion dynamique des écrans sous Hyprland",
    long_about = "hyprmc détecte les écrans, les positionne, les tourne ou les inverse, \
                  et maintient la configuration à jour au branchement à chaud.\n\
                  Il expose une interface web pour faire tout cela à la souris."
)]
pub struct Cli {
    /// Journalisation détaillée.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Liste les écrans détectés.
    List {
        /// Sortie JSON brute.
        #[arg(long)]
        json: bool,
    },

    /// Affiche les modes disponibles d'un écran.
    Modes {
        /// Nom du connecteur (`eDP-1`, `DP-1`…).
        output: String,
    },

    /// Modifie un écran et applique le changement immédiatement.
    Set(SetArgs),

    /// Positionne des écrans les uns par rapport aux autres.
    ///
    /// Exemple : `hyprmc arrange DP-1 right-of eDP-1`
    /// Relations : left-of, right-of, above, below, same-as.
    Arrange {
        /// Suite de triplets « ÉCRAN RELATION RÉFÉRENCE ».
        #[arg(required = true, num_args = 3..)]
        spec: Vec<String>,

        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Range automatiquement les écrans de gauche à droite.
    Auto {
        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Gestion des profils.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Applique le profil correspondant aux écrans branchés.
    Apply {
        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Écrit l'agencement courant dans `monitors.conf`.
    Persist,

    /// Branche `monitors.conf` dans `hyprland.conf`.
    Init {
        /// Montre ce qui serait fait sans rien modifier.
        #[arg(long)]
        dry_run: bool,
    },

    /// Démon : réagit au branchement à chaud et sert l'interface web.
    Daemon {
        #[command(flatten)]
        web: WebArgs,

        /// Ne pas démarrer l'interface web.
        #[arg(long)]
        no_web: bool,
    },

    /// Interface web seule, sans surveillance du branchement à chaud.
    Web {
        #[command(flatten)]
        web: WebArgs,
    },
}

#[derive(Debug, Args)]
pub struct WebArgs {
    /// Port d'écoute (défaut : celui de la configuration, 8787).
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Adresse d'écoute (défaut : 127.0.0.1).
    #[arg(long)]
    pub bind: Option<String>,
}

#[derive(Debug, Args, Clone, Copy)]
pub struct SafetyArgs {
    /// Applique malgré les erreurs de validation et les écarts constatés.
    #[arg(long)]
    pub force: bool,

    /// Pas de demande de confirmation ni de retour arrière automatique.
    #[arg(long)]
    pub no_confirm: bool,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Nom du connecteur à modifier.
    pub output: String,

    /// Mode : `1920x1080@60`, ou `preferred`.
    #[arg(short, long)]
    pub mode: Option<String>,

    /// Position dans l'espace de travail : `1920x0`.
    #[arg(short, long)]
    pub pos: Option<String>,

    /// Facteur d'échelle.
    #[arg(short, long)]
    pub scale: Option<f64>,

    /// Rotation en degrés : 0, 90, 180 ou 270.
    #[arg(short, long, value_parser = ["0", "90", "180", "270"])]
    pub rotate: Option<String>,

    /// Inverse l'image (effet miroir horizontal).
    #[arg(long, conflicts_with = "no_flip")]
    pub flip: bool,

    /// Rétablit une image non inversée.
    #[arg(long)]
    pub no_flip: bool,

    /// Duplique l'écran indiqué.
    #[arg(long, conflicts_with = "no_mirror")]
    pub mirror: Option<String>,

    /// Cesse de dupliquer un autre écran.
    #[arg(long)]
    pub no_mirror: bool,

    /// Active l'écran.
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,

    /// Désactive l'écran.
    #[arg(long)]
    pub disable: bool,

    /// Rafraîchissement variable.
    #[arg(long, value_parser = parse_onoff)]
    pub vrr: Option<bool>,

    /// Enregistre le résultat dans le profil indiqué.
    #[arg(long, value_name = "PROFIL")]
    pub save: Option<String>,

    #[command(flatten)]
    pub safety: SafetyArgs,
}

fn parse_onoff(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "oui" => Ok(true),
        "off" | "false" | "0" | "non" => Ok(false),
        other => Err(format!("valeur attendue : on ou off (reçu « {other} »)")),
    }
}

#[derive(Debug, Subcommand)]
pub enum ProfileAction {
    /// Liste les profils enregistrés.
    List,

    /// Affiche le détail d'un profil.
    Show { name: String },

    /// Enregistre l'agencement courant sous ce nom.
    Save {
        name: String,

        /// Le profil ne s'appliquera que si aucun autre écran n'est branché.
        #[arg(long)]
        exact: bool,
    },

    /// Applique un profil.
    Apply {
        name: String,

        #[command(flatten)]
        safety: SafetyArgs,
    },

    /// Supprime un profil.
    Delete { name: String },

    /// Renomme un profil.
    Rename { from: String, to: String },
}
