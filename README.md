# 🧛 Buffy contre les Vampires — Bot Discord AI Dungeon (Rust)

Ce projet est un bot Discord codé en Rust qui permet de jouer à une aventure textuelle de type "AI Dungeon" en co-écriture avec des amis. L'histoire se déroule dans l'univers de *Buffy contre les vampires* : Sunnydale, la Bouche de l'Enfer, le Conseil des Observateurs, les vampires, les démons et les sorts qui tournent mal.

Le bot utilise l'API **Gemini** ou **Mistral** pour la narration (avec un schéma JSON structuré pour garantir un comportement fiable). Le modèle peut être changé en cours de partie avec `/model` ; ce choix est sauvegardé avec l'état du jeu et survit aux redémarrages.

---

## 🛠️ Fonctionnalités du Bot

- **Narrateur interactif** : le LLM joue le rôle d'Observateur / Maître de Jeu et décrit les conséquences de vos actions, dés à 20 faces à l'appui.
- **Mode multijoueur** : une aventure par **salon Discord**. Tous les joueurs d'un même salon partagent la même histoire et jouent à tour de rôle.
- **Un joueur, un personnage** : chaque joueur annonce qui il incarne, et le bot traduit son « je » par le nom du personnage pour le narrateur.
- **Rattrapage après une coupure** : au redémarrage, le bot relit l'historique du salon et joue les actions reçues pendant son absence.
- **Slash commands** :
  - `/start [etat]` : Démarre une nouvelle aventure. Vous pouvez spécifier un état initial (ex : `/start etat:Nous sommes au cimetière, il fait nuit`), par exemple obtenu via `/summary`.
  - `/personnage [nom]` : Annonce le personnage que vous incarnez (texte libre) ; sans argument, rappelle qui vous êtes et propose des personnages.
  - `/summary` : Génère le journal de l'Observateur, réutilisable comme état initial de `/start` (sur ce bot ou ailleurs).
  - `/model gemini|mistral` : Change le modèle utilisé pour la suite de l'aventure (nécessite la clé API correspondante).
  - `/deploy [force]` : Déploie la dernière version du bot (réservé à l'admin, voir `ADMIN_USER_ID`).
  - `/help` : Affiche l'aide et les commandes.

### Qui est « je » ? — personnages et règle d'action

Pour éviter la confusion entre le joueur, son personnage et les autres, chaque joueur doit **s'annoncer avant de pouvoir faire avancer l'histoire** :

1. Tapez `/personnage Buffy` (le nom est du texte libre : un personnage de la série ou le vôtre). Le bot mémorise que votre identifiant Discord correspond à ce personnage, et cette association est sauvegardée avec la partie.
2. Tant que vous ne l'avez pas fait, **vos messages sont refusés** et le bot vous renvoie vers `/personnage`. Rien n'est deviné à partir du texte de vos messages : l'identité ne se déclare que par la commande.
3. `/personnage` sans nom rappelle qui vous incarnez, qui sont les autres joueurs, et propose quelques figures de Sunnydale (Buffy, Giles, Willow, Angel…) avec une courte présentation.
4. Une fois annoncé, écrivez vos actions à la première personne : « Je pousse la porte » est transmis au narrateur comme « **Buffy** pousse la porte ».

Un joueur n'agit que **par son personnage** :

- ✅ « Je prends le bras de Giles, il est stupéfait, et il se met à pleuvoir dehors » — l'action est celle de votre personnage ; le décor, l'ambiance et les réactions des PNJ restent libres.
- ❌ « Giles passe la porte » — aucune action de votre personnage : la demande est refusée.
- ❌ Faire agir ou parler le personnage d'un **autre joueur** : refusé également.

Un même personnage ne peut être incarné que par un seul joueur, et `/personnage` sert aussi à en changer en cours de partie. Les personnages déclarés sont conservés lors d'un nouveau `/start`.

### Comment le bot lit les messages

Une fois `/start` lancé dans un salon, **tout message normal du salon est interprété comme une action de jeu**. Pour discuter entre joueurs sans que le bot ne réagisse :

- préfixez le message par `/ignore`, `/i ` ou `!` ;
- ou **répondez** (reply) à un message : les réponses ne sont jamais interprétées comme des actions.

Tant qu'aucune aventure n'a été lancée dans un salon, le bot reste totalement silencieux.

Un message qui commence par `/` n'est jamais joué comme une action : Discord envoie une commande comme un message ordinaire quand son client ne la connaît pas encore (une commande fraîchement déployée met un moment à apparaître). `/personnage [nom]` et `/help` tapés en toutes lettres sont donc traités comme les slash commands correspondantes ; les autres reçoivent un rappel. Au démarrage, les commandes sont aussi enregistrées serveur par serveur, où elles sont visibles immédiatement.

### Messages reçus pendant une coupure

Discord ne rejoue pas les messages envoyés pendant qu'un bot est hors ligne. Le bot mémorise donc, dans la session du salon, l'identifiant du dernier message traité ; à chaque (re)connexion, il relit l'historique du salon depuis ce marqueur et joue les actions manquées. Le rattrapage ne s'annonce pas dans le salon : les joueurs voient simplement la suite de l'histoire arriver (le détail est écrit dans les logs).

Deux garde-fous évitent de noyer le salon après une longue panne (réglables dans le `.env`) :

- `CATCHUP_LIMIT` (défaut : 20) : nombre maximum d'actions rejouées par salon ; seules les plus récentes sont jouées.
- `CATCHUP_MAX_AGE_HOURS` (défaut : 24) : les messages plus anciens sont ignorés.

Le rattrapage nécessite la permission Discord **Read Message History**. Les sessions créées avant cette fonctionnalité n'ont pas de marqueur : le bot se cale alors sur le présent sans rejouer l'historique.

---

## 📋 Prérequis

1. **Rust** installé sur votre machine (développement) ou sur le Raspberry Pi.
2. **Une application Discord + un bot** (créés sur https://discord.com/developers/applications).
3. **Une clé API Gemini** (obtenue sur Google AI Studio) ou **Mistral**.

---

## ⚙️ Configuration

### 1. Créer le bot Discord

1. Sur https://discord.com/developers/applications, créez une application, puis un **Bot**.
2. Copiez le **token** du bot (onglet *Bot* → *Reset Token*).
3. Dans l'onglet *Bot*, activez l'intent privilégié **MESSAGE CONTENT INTENT**.
   > [!IMPORTANT]
   > Sans cet intent, Discord n'envoie pas le contenu des messages : le bot verra les slash commands mais **aucune action de jeu**. Un avertissement est écrit dans les logs si ce cas est détecté.
4. Onglet *OAuth2 → URL Generator* : cochez les scopes `bot` et `applications.commands`, puis les permissions `Send Messages` et `Read Message History`. Ouvrez l'URL générée pour inviter le bot sur votre serveur.

### 2. Le fichier `.env`

Créez un fichier nommé `.env` à la racine du projet :

```env
# Token du bot Discord (onglet Bot du portail développeur)
DISCORD_TOKEN=ton_token_discord_ici

# Modèle utilisé par défaut : "gemini" ou "mistral"
MODEL_PROVIDER=gemini

# Clé API Gemini (obtenue sur Google AI Studio)
GEMINI_API_KEY=ta_cle_gemini_ici

# Clé API Mistral (facultative, nécessaire pour /model mistral)
MISTRAL_API_KEY=ta_cle_mistral_ici

# ID Discord de l'administrateur autorisé à lancer /deploy (facultatif).
# Pour l'obtenir : activez le mode développeur dans Discord, puis clic droit
# sur votre nom > "Copier l'identifiant".
ADMIN_USER_ID=123456789012345678

# Chemin vers cargo, uniquement si /deploy ne le trouve pas (facultatif).
# Par défaut : $CARGO_BIN, puis $HOME/.cargo/bin/cargo, puis le PATH.
#CARGO_BIN=/home/utilisateur/.cargo/bin/cargo

# Nom du service systemd redémarré par /deploy (facultatif, défaut: raspberry-bot)
#SERVICE_NAME=raspberry-bot

# Rattrapage des messages reçus pendant que le bot était hors ligne (facultatif).
# Nombre maximum d'actions rejouées par salon, et âge maximum d'un message rejoué.
#CATCHUP_LIMIT=20
#CATCHUP_MAX_AGE_HOURS=24

# Niveau de log pour le bot. Les spans internes Serenity (heartbeats, gateway)
# sont limités à WARN par le programme pour ne pas polluer le journal.
RUST_LOG=info
```

> Les slash commands sont enregistrées globalement au démarrage du bot. Discord peut mettre quelques minutes à les propager la première fois.

---

## 🚀 Lancement local (Développement)

Pour tester le bot et vérifier que tout fonctionne :

```bash
cargo run
```

---

## 📦 Déploiement permanent sur Raspberry Pi

Pour faire tourner le bot de manière stable et permanente sur votre Raspberry Pi :

### 1. Compiler en mode Release
La compilation sur Raspberry Pi peut prendre quelques minutes mais produira un exécutable optimisé.
```bash
cargo build --release
```
L'exécutable compilé se trouvera dans `target/release/raspberry_bot`.

### 2. Configurer un service Systemd (Recommandé)

Le plus simple est d'utiliser le script fourni, qui crée le service avec le bon utilisateur et le bon répertoire de travail :

```bash
sudo ./install_service.sh
```

Il génère un fichier `/etc/systemd/system/raspberry-bot.service` équivalent à :

```ini
[Unit]
Description=Buffy the Vampire Slayer Discord Bot
After=network.target

[Service]
Type=simple
User=<votre utilisateur>
WorkingDirectory=<répertoire du projet>
ExecStart=<répertoire du projet>/target/release/raspberry_bot
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Pour voir les logs du bot en temps réel :
```bash
sudo journalctl -u raspberry-bot.service -f
```

Pour retirer le service :
```bash
sudo ./uninstall_service.sh
```

> [!NOTE]
> La commande `/deploy` exécute `git pull`, `cargo build --release` puis `sudo systemctl restart raspberry-bot`. Pour qu'elle fonctionne sans mot de passe, l'utilisateur du service doit avoir une règle sudoers autorisant ce redémarrage.

---

## 🔍 Structure du Code

- [Cargo.toml](Cargo.toml) : Gère les dépendances (Serenity, Reqwest, Serde, Tokio).
- [src/common.rs](src/common.rs) : État de la conversation (personnages déclarés, marqueur de rattrapage), consigne système (l'univers Buffy, les règles d'identité), et orchestration partagée (résumé glissant, génération de l'histoire) indépendante du provider.
- [src/gemini.rs](src/gemini.rs) : Adaptateur pour l'API REST de Gemini (génération de texte structuré JSON).
- [src/mistral.rs](src/mistral.rs) : Adaptateur pour l'API REST de Mistral.
- [src/main.rs](src/main.rs) : Logique du bot Discord (Serenity) — slash commands, actions de jeu, déclaration des personnages, rattrapage des messages manqués, persistance des sessions par salon, déploiement.

Les sessions sont stockées dans le dossier `sessions/`, un fichier JSON par salon Discord.
