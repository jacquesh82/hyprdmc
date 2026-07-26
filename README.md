# hyprmc

**Gestion dynamique des écrans pour [Hyprland](https://hyprland.org/)** — CLI et interface web,
dans un binaire unique.

Hyprland ne gère pas le branchement à chaud des écrans : chaque changement de configuration
(dock, projecteur, écran externe) suppose d'éditer `hyprland.conf` à la main puis de recharger.
`hyprmc` détecte les écrans, les positionne, les tourne, les inverse — et surtout **réapplique
tout seul le bon profil** quand le matériel change.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Écran   État            Mode              Position   Échelle   Orientation   │
╞══════════════════════════════════════════════════════════════════════════════╡
│ eDP-1   actif (focus)   1920x1080@60.06   0x0        1         0°            │
│ DP-3    actif           3840x2160@60.00   1920x0     1.5       90° inversé   │
└──────────────────────────────────────────────────────────────────────────────┘
```

## Sommaire

- [Fonctionnalités](#fonctionnalités)
- [Installation](#installation)
- [Démarrage rapide](#démarrage-rapide)
- [Interface web](#interface-web)
- [Ligne de commande](#ligne-de-commande)
- [Profils](#profils)
- [Démon et branchement à chaud](#démon-et-branchement-à-chaud)
- [Persistance](#persistance)
- [Rotation et inversement](#rotation-et-inversement)
- [Filet de sécurité](#filet-de-sécurité)
- [API HTTP](#api-http)
- [Configuration](#configuration)
- [Développement](#développement)
- [Fonctionnement interne](#fonctionnement-interne)
- [Dépannage](#dépannage)
- [Licence](#licence)

## Fonctionnalités

- **Détection** — liste les écrans branchés, leurs modes, leur position, leur orientation.
- **Positionnement** — au pixel près, par relations (`right-of`, `below`…), ou rangement
  automatique.
- **Rotation et inversement** — 0/90/180/270°, avec ou sans effet miroir.
- **Duplication** — un écran peut recopier l'image d'un autre.
- **Interface web** — canevas glisser-déposer avec aimantation, dans le navigateur.
- **Profils** — un agencement par situation, identifié par les écrans branchés.
- **Branchement à chaud** — un démon écoute Hyprland et applique le bon profil sans intervention.
- **Filet de sécurité** — toute modification revient en arrière automatiquement sans confirmation.
- **Persistance** — génère `monitors.conf` sans jamais réécrire votre `hyprland.conf`.
- **Sans dépendance** — parle directement aux sockets de Hyprland, `hyprctl` n'est pas requis.
  Aucun toolchain JavaScript : l'interface web est embarquée dans le binaire.

## Installation

### Depuis les sources

```sh
git clone https://github.com/jacquesh82/hyprmc.git
cd hyprmc
cargo build --release
install -Dm755 target/release/hyprmc ~/.local/bin/hyprmc
```

Rust 1.87 ou plus récent (édition 2024). Hyprland 0.40+ ; développé et testé sur 0.56.

### Avec cargo

```sh
cargo install --path .
```

## Démarrage rapide

```sh
# 1. Que voit-on ?
hyprmc list

# 2. Arranger les écrans
hyprmc arrange DP-1 right-of eDP-1

# 3. Enregistrer la situation actuelle sous un nom
hyprmc profile save bureau

# 4. Brancher monitors.conf dans hyprland.conf (sauvegarde automatique)
hyprmc init

# 5. Lancer le démon : hotplug + interface web sur http://127.0.0.1:8787
hyprmc daemon
```

## Interface web

```sh
hyprmc web            # interface seule
hyprmc daemon         # interface + surveillance du branchement à chaud
```

Puis <http://127.0.0.1:8787>.

- Glissez les écrans sur le canevas : ils **s'aimantent** aux bords voisins.
- Flèches du clavier pour un réglage fin (`Maj` pour des pas de 100 px).
- Panneau latéral : mode, échelle, rotation, inversement, duplication, VRR, activation.
- Les chevauchements sont signalés en rouge et bloquent le bouton **Appliquer**.
- Après application, un bandeau propose de **conserver** ou de **revenir en arrière** ;
  sans réponse, la configuration précédente est restaurée automatiquement.
- L'état se met à jour en temps réel (SSE) quand un écran est branché ou débranché.

L'écoute est limitée à `127.0.0.1` par défaut. Pour l'ouvrir à votre réseau local — en toute
connaissance de cause, l'API n'a **aucune authentification** :

```sh
hyprmc web --bind 0.0.0.0 --port 8787
```

## Ligne de commande

### Lecture

```sh
hyprmc list                  # tableau des écrans
hyprmc list --json           # sortie brute
hyprmc modes eDP-1           # modes disponibles
```

### Modification

```sh
hyprmc set DP-1 --mode 3840x2160@60 --scale 1.5
hyprmc set DP-1 --rotate 90              # portrait
hyprmc set DP-1 --rotate 90 --flip       # portrait, image inversée
hyprmc set DP-1 --pos 1920x0
hyprmc set DP-1 --mirror eDP-1           # duplique l'écran du portable
hyprmc set DP-1 --no-mirror
hyprmc set eDP-1 --disable               # portable fermé sur le dock
hyprmc set DP-1 --vrr on
hyprmc set DP-1 --rotate 270 --save bureau   # applique et enregistre dans le profil
```

### Positionnement relatif

```sh
hyprmc arrange DP-1 right-of eDP-1
hyprmc arrange DP-1 above eDP-1 DP-2 right-of DP-1     # plusieurs triplets
hyprmc auto                                            # rangement horizontal
```

Relations : `left-of`, `right-of`, `above`, `below`, `same-as`
(alias français : `gauche-de`, `droite-de`, `au-dessus-de`, `en-dessous-de`).

### Options communes

| Option | Effet |
|---|---|
| `--force` | applique malgré les avertissements et les écarts constatés |
| `--no-confirm` | pas de demande de confirmation ni de retour arrière |
| `-v`, `--verbose` | journalisation détaillée |

## Profils

Un profil décrit un agencement et la manière de reconnaître les écrans auxquels il s'applique.

```sh
hyprmc profile save bureau          # enregistre l'agencement courant
hyprmc profile save solo --exact    # ne s'applique que si aucun autre écran n'est branché
hyprmc profile list
hyprmc profile show bureau
hyprmc profile apply bureau
hyprmc profile rename bureau dock
hyprmc profile delete dock
hyprmc apply                        # applique le profil correspondant au matériel présent
```

### Reconnaissance des écrans

Un écran est désigné par son **empreinte** — `fabricant modèle numéro-de-série` — et non par son
connecteur : rebrancher le même écran sur un autre port ne casse pas le profil. Le nom du
connecteur reste accepté, ainsi que les motifs avec `*` :

```toml
match = "Dell Inc. U2723QE H7X2K93"    # empreinte complète, sans ambiguïté
match = "Dell*"                        # n'importe quel Dell
match = "eDP-1"                        # par connecteur
```

### Choix du profil

Parmi les profils dont **toutes** les règles trouvent un écran branché, `hyprmc` retient celui qui
couvre le plus d'écrans. À égalité, un profil `exact` l'emporte, puis le premier déclaré.

Un écran branché que le profil ne mentionne pas n'est pas perdu : il est activé dans son mode
préféré et posé à droite de l'agencement.

Si aucun profil ne correspond, les écrans sont simplement rangés de gauche à droite.

## Démon et branchement à chaud

```sh
hyprmc daemon                    # hotplug + interface web
hyprmc daemon --no-web           # hotplug seul
hyprmc daemon --port 9000
```

Le démon écoute le socket d'événements de Hyprland. À chaque branchement ou débranchement, il
attend 500 ms que la situation se stabilise — un dock émet plusieurs événements d'affilée — puis
sélectionne et applique le profil correspondant. Il se reconnecte tout seul si Hyprland redémarre.

### Démarrage automatique

Avec Hyprland, dans `hyprland.conf` :

```conf
exec-once = hyprmc daemon
```

Ou en service utilisateur systemd — `~/.config/systemd/user/hyprmc.service` :

```ini
[Unit]
Description=Gestion dynamique des écrans Hyprland
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/hyprmc daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now hyprmc.service
```

## Persistance

`hyprmc` ne réécrit jamais votre `hyprland.conf`. Il gère son propre fichier,
`~/.config/hypr/monitors.conf`, et l'y branche une seule fois :

```sh
hyprmc init --dry-run     # montre ce qui serait fait
hyprmc init               # sauvegarde, puis modifie
```

`init` est idempotent et :

1. copie `hyprland.conf` en `hyprland.conf.hyprmc.bak` ;
2. commente les directives `monitor =` existantes, en les reprenant dans `monitors.conf` ;
3. insère `source = ~/.config/hypr/monitors.conf` après vos autres `source`.

Ensuite, `hyprmc persist` réécrit `monitors.conf` depuis l'état courant. Le fichier est écrit de
façon atomique : Hyprland ne peut jamais en lire une version partielle.

```conf
# Généré par hyprmc — NE PAS ÉDITER À LA MAIN.
monitor = eDP-1,1920x1080@60.06,0x0,1
monitor = DP-3,3840x2160@60.00,1920x0,1.5,transform,5
```

## Rotation et inversement

Hyprland encode l'orientation dans un entier unique. `hyprmc` expose deux réglages indépendants et
fait la conversion :

| `transform` | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
|---|---|---|---|---|---|---|---|---|
| **rotation** | 0° | 90° | 180° | 270° | 0° | 90° | 180° | 270° |
| **inversé** | non | non | non | non | **oui** | **oui** | **oui** | **oui** |

```sh
hyprmc set DP-1 --rotate 90              # transform 1
hyprmc set DP-1 --rotate 90 --flip       # transform 5
hyprmc set DP-1 --rotate 0 --no-flip     # transform 0
```

La rotation échange largeur et hauteur dans l'espace de travail : `hyprmc` en tient compte pour
positionner les écrans et détecter les chevauchements.

## Filet de sécurité

Une rotation malheureuse ou une position aberrante peut rendre un écran illisible. Trois garde-fous :

1. **Validation avant envoi** — chevauchements, écrans inatteignables, échelles impossibles,
   duplication d'un écran absent, extinction de tous les écrans. Les erreurs bloquent, les
   avertissements informent. `--force` passe outre.

2. **Vérification après envoi** — Hyprland répond `ok` même quand il n'obéit pas : un mode
   inexistant est accepté en silence, une échelle invalide est arrondie sans le dire. `hyprmc`
   relit l'état et compare. Si le résultat ne correspond pas, il revient immédiatement en arrière.

3. **Confirmation différée** — après une application réussie, vous avez 10 secondes pour confirmer.
   Sans réponse, la configuration précédente est restaurée. Ne rien faire suffit à s'en sortir.

```sh
hyprmc set DP-1 --rotate 90
# Conserver cette configuration ? [o/N] (retour arrière automatique dans 10 s)
```

Le délai se règle par `confirm_timeout_secs` ; `0` désactive le mécanisme, `--no-confirm` le
contourne ponctuellement.

## API HTTP

| Méthode | Route | Rôle |
|---|---|---|
| `GET` | `/api/state` | état complet : écrans, agencement, anomalies, profils |
| `GET` | `/api/monitors` | écrans bruts tels que rapportés par Hyprland |
| `POST` | `/api/apply` | applique un agencement (`{outputs, force, guard}`) |
| `POST` | `/api/confirm` | confirme la dernière application |
| `POST` | `/api/revert` | revient immédiatement en arrière |
| `POST` | `/api/persist` | écrit `monitors.conf` |
| `GET` | `/api/profiles` | liste des profils et profil actif |
| `PUT` | `/api/profiles/{nom}` | enregistre un profil |
| `DELETE` | `/api/profiles/{nom}` | supprime un profil |
| `POST` | `/api/profiles/{nom}/apply` | applique un profil |
| `GET` | `/api/events` | flux SSE poussant l'état à chaque changement |

```sh
curl -s localhost:8787/api/state | jq '.monitors[].name'
curl -X POST localhost:8787/api/profiles/bureau/apply
```

## Configuration

`~/.config/hyprmc/config.toml` (créé au premier `profile save`) :

```toml
[settings]
web_port = 8787
bind = "127.0.0.1"
auto_apply = true               # le démon applique le profil au branchement
confirm_timeout_secs = 10       # 0 = pas de retour arrière automatique
monitors_conf = "/home/vous/.config/hypr/monitors.conf"

[[profile]]
name = "bureau"
exact = false

[[profile.output]]
match = "AU Optronics 0x5799"   # dalle du portable
enabled = false                 # capot fermé sur le dock

[[profile.output]]
match = "Dell Inc. U2723QE H7X2K93"
mode = "3840x2160@60"
position = "0x0"
scale = 1.5
rotation = 0
flipped = false
vrr = true
```

Champs d'une règle : `match` (obligatoire), `enabled`, `mode`, `position`, `scale`, `rotation`,
`flipped`, `mirror_of`, `vrr`. `mode` et `position` acceptent `"auto"` pour laisser `hyprmc`
choisir.

Journalisation : `HYPRMC_LOG=hyprmc=debug hyprmc daemon`.

## Développement

```sh
cargo test              # 99 tests, sans Hyprland requis
cargo clippy --all-targets
cargo fmt
```

La logique métier ne dépend que du trait `HyprBackend`, ce qui permet de tout tester avec un
backend simulé — y compris la latence d'application du compositeur.

### Tester le multi-écrans sans matériel

Hyprland sait créer des sorties virtuelles :

```sh
hyprctl output create headless test-1
hyprmc list
hyprmc set test-1 --rotate 90 --no-confirm
hyprctl output remove test-1
```

Pour ne pas toucher à votre vraie configuration pendant les essais :

```sh
XDG_CONFIG_HOME=/tmp/essai hyprmc profile save brouillon
```

## Fonctionnement interne

```
src/
  ipc.rs       sockets Hyprland : requêtes (.socket.sock) et événements (.socket2.sock)
  monitor.rs   modèle d'un écran, rotation, modes, empreinte
  layout.rs    agencement, tailles logiques, validation, arrangement
  apply.rs     envoi en lot, vérification, retour arrière
  config.rs    profils TOML, correspondance avec le matériel
  emit.rs      génération de monitors.conf, branchement dans hyprland.conf
  daemon.rs    boucle d'événements, anti-rebond, état partagé
  web/         API axum, flux SSE, interface embarquée
```

Trois comportements de Hyprland ont façonné la conception, tous vérifiés sur la version 0.56 :

- **`ok` ne veut pas dire « fait »** — un mode inexistant, une position aberrante ou une échelle
  invalide sont acceptés sans erreur. D'où la relecture systématique de l'état.
- **L'application est asynchrone** — une rotation met une cinquantaine de millisecondes à se
  refléter dans `j/monitors`. `hyprmc` relit jusqu'à convergence plutôt qu'une seule fois, sans
  attendre inutilement sur les corrections que le compositeur ne reprendra jamais.
- **`mirrorOf` contient un identifiant, pas un nom** — Hyprland publie `"0"` là où la
  configuration attend `eDP-1`. La résolution est faite à la lecture.

## Dépannage

**« Hyprland ne semble pas accessible »**
`HYPRLAND_INSTANCE_SIGNATURE` n'est pas défini (service systemd lancé trop tôt, session
distante…). Le démon retrouve l'instance tout seul s'il n'y en a qu'une ; sinon, exportez la
variable.

**Mon échelle est modifiée toute seule**
Hyprland n'accepte que les échelles donnant une taille logique entière. `hyprmc` prévient avant
l'envoi et suggère la valeur valide la plus proche.

**Le profil ne s'applique pas au branchement**
Vérifiez que le démon tourne (`systemctl --user status hyprmc`), que `auto_apply` est à `true`,
et que le profil correspond : `hyprmc profile list` indique quels profils sont compatibles avec le
matériel présent.

**Mes réglages disparaissent au redémarrage de Hyprland**
Lancez `hyprmc init` puis `hyprmc persist` : sans cela, les modifications ne vivent qu'en mémoire.

## Licence

MIT — voir [LICENSE](LICENSE).
