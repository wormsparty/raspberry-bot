# 🧛 Buffy contre les Vampires — Bot Discord AI Dungeon (Rust)

Ce projet est un bot Discord codé en Rust qui permet de jouer à une aventure textuelle de type "AI Dungeon" en co-écriture avec des amis. L'histoire se déroule dans l'univers de *Buffy contre les vampires* : Sunnydale, la Bouche de l'Enfer, le Conseil des Observateurs, les vampires, les démons et les sorts qui tournent mal.

Le bot utilise l'API **Gemini** ou **Mistral** pour la narration (avec un schéma JSON structuré pour garantir un comportement fiable). Le modèle peut être changé en cours de partie avec `/model` ; ce choix est sauvegardé avec l'état du jeu et survit aux redémarrages.

---

## 🛠️ Fonctionnalités du Bot

- **Narrateur interactif** : le LLM joue le rôle d'Observateur / Maître de Jeu et décrit les conséquences de vos actions, dés à 20 faces à l'appui.
- **Mode multijoueur** : une aventure par **salon Discord**. Tous les joueurs d'un même salon partagent la même histoire et jouent à tour de rôle.
- **Illustrations optionnelles** : pour les scènes visuellement marquantes, le bot génère une image via OpenRouter (nécessite `OPENROUTER_API_KEY`).
- **Slash commands** :
  - `/start [etat]` : Démarre une nouvelle aventure. Vous pouvez spécifier un état initial (ex : `/start etat:Nous sommes au cimetière, il fait nuit`), par exemple obtenu via `/summary`.
  - `/summary` : Génère le journal de l'Observateur, réutilisable comme état initial de `/start` (sur ce bot ou ailleurs).
  - `/model gemini|mistral` : Change le modèle utilisé pour la suite de l'aventure (nécessite la clé API correspondante).
  - `/image actif:true|false` : Active ou désactive la génération d'images.
  - `/deploy [force]` : Déploie la dernière version du bot (réservé à l'admin, voir `ADMIN_USER_ID`).
  - `/help` : Affiche l'aide et les commandes.

### Comment le bot lit les messages

Une fois `/start` lancé dans un salon, **tout message normal du salon est interprété comme une action de jeu**. Pour discuter entre joueurs sans que le bot ne réagisse :

- préfixez le message par `/ignore`, `/i ` ou `!` ;
- ou **répondez** (reply) à un message : les réponses ne sont jamais interprétées comme des actions.

Tant qu'aucune aventure n'a été lancée dans un salon, le bot reste totalement silencieux.

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
4. Onglet *OAuth2 → URL Generator* : cochez les scopes `bot` et `applications.commands`, puis les permissions `Send Messages`, `Attach Files` et `Read Message History`. Ouvrez l'URL générée pour inviter le bot sur votre serveur.

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

# Clé OpenRouter (facultative, nécessaire pour la génération d'images)
OPENROUTER_API_KEY=ta_cle_openrouter_ici

# ID Discord de l'administrateur autorisé à lancer /deploy (facultatif).
# Pour l'obtenir : activez le mode développeur dans Discord, puis clic droit
# sur votre nom > "Copier l'identifiant".
ADMIN_USER_ID=123456789012345678

# Chemin vers cargo, uniquement si /deploy ne le trouve pas (facultatif).
# Par défaut : $CARGO_BIN, puis $HOME/.cargo/bin/cargo, puis le PATH.
#CARGO_BIN=/home/utilisateur/.cargo/bin/cargo

# Nom du service systemd redémarré par /deploy (facultatif, défaut: raspberry-bot)
#SERVICE_NAME=raspberry-bot

# Niveau de log pour la console
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
- [src/common.rs](src/common.rs) : État de la conversation, consigne système (l'univers Buffy), et orchestration partagée (résumé glissant, génération de l'histoire) indépendante du provider.
- [src/gemini.rs](src/gemini.rs) : Adaptateur pour l'API REST de Gemini (génération de texte structuré JSON).
- [src/mistral.rs](src/mistral.rs) : Adaptateur pour l'API REST de Mistral.
- [src/image.rs](src/image.rs) : Génération d'illustrations via OpenRouter.
- [src/main.rs](src/main.rs) : Logique du bot Discord (Serenity) — slash commands, actions de jeu, persistance des sessions par salon, déploiement.

Les sessions sont stockées dans le dossier `sessions/`, un fichier JSON par salon Discord.
