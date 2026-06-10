# 🛸 Mulder & Scully - Telegram AI Dungeon Bot (Rust)

Ce projet est un exemple de bot Telegram codé en Rust qui permet de jouer à une aventure textuelle de type "AI Dungeon" en co-écriture avec un ami. L'histoire suit les agents Fox Mulder et Dana Scully dans des enquêtes humoristiques, absurdes et surréalistes (maisons marchantes, tableaux de Freud récurrents, etc.). 

Le bot utilise l'API **Gemini** ou **Mistral** pour la narration (avec un schéma JSON structuré pour garantir un comportement fiable). Le modèle peut être changé en cours de partie avec `/gemini` et `/mistral` ; ce choix est sauvegardé avec l'état du jeu et survit aux redémarrages.

---

## 🛠️ Fonctionnalités du Bot

- **Narrateur interactif** : le LLM joue le rôle de Maître de Jeu (MJ) et décrit les conséquences absurdes de vos actions.
- **Mode multijoueur (Groupes)** : Ajoutez le bot dans un groupe avec votre ami. Le bot maintient l'état de l'histoire pour le groupe entier, permettant de jouer à deux en prenant des tours.
- **Commandes intégrées** :
  - `/start [état initial]` : Démarre une nouvelle enquête absurde. Vous pouvez lui spécifier un état initial (ex : `/start Nous sommes au pôle nord, il fait froid`), par exemple obtenu via `/summary`.
  - `/summary` : Génère un résumé complet de l'enquête, réutilisable comme état initial de `/start` (sur ce bot ou ailleurs).
  - `/gemini` / `/mistral` : Change le modèle utilisé pour la suite de l'enquête (nécessite la clé API correspondante).
  - `/help` : Affiche l'aide et les commandes.
  - Préfixez un message par `/ignore` pour parler aux autres joueurs sans que le bot ne réagisse.

---

## 📋 Prérequis

1. **Rust** installé sur votre machine (développement) ou sur le Raspberry Pi.
2. **Un Token de Bot Telegram** (créé via `@BotFather`).
3. **Une clé API Gemini** (obtenue sur Google AI Studio).

---

## ⚙️ Configuration

1. Clonez ou copiez ce dossier sur votre Raspberry Pi (ou machine de dev).
2. Créez un fichier nommé `.env` à la racine du projet et ajoutez vos clés :

```env
# Token Telegram (obtenu auprès de @BotFather)
TELEGRAM_BOT_TOKEN=ton_token_telegram_ici

# Modèle utilisé par défaut : "gemini" ou "mistral"
MODEL_PROVIDER=gemini

# Clé API Gemini (obtenue sur Google AI Studio)
GEMINI_API_KEY=ta_cle_gemini_ici

# Clé API Mistral (facultative, nécessaire pour /mistral)
MISTRAL_API_KEY=ta_cle_mistral_ici

# Niveau de log pour la console
RUST_LOG=info
```

> [!IMPORTANT]
> **Pour jouer dans un groupe Telegram avec votre ami :**
> Par défaut, les bots Telegram ne lisent pas tous les messages d'un groupe (pour des raisons de confidentialité).
> Pour que le bot puisse réagir à chaque action de l'histoire dans un groupe, vous devez :
> 1. Ouvrir une discussion avec `@BotFather`.
> 2. Envoyer la commande `/setprivacy`.
> 3. Sélectionner votre bot.
> 4. Choisir **Disable** (Désactiver).
> *(Alternativement, vous pouvez simplement promouvoir le bot en tant qu'administrateur du groupe)*.

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
L'exécutable compilé se trouvera dans `target/release/xfiles_bot`.

### 2. Configurer un service Systemd (Recommandé)
Pour que le bot se lance automatiquement au démarrage du Raspberry Pi et redémarre en cas de plantage :

Créez un fichier de service systemd (par exemple `/etc/systemd/system/xfiles-bot.service`) avec les droits super-utilisateur (`sudo nano /etc/systemd/system/xfiles-bot.service`) :

```ini
[Unit]
Description=Mulder and Scully Telegram Bot
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi/xfiles_bot
ExecStart=/home/pi/xfiles_bot/target/release/xfiles_bot
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Activez et démarrez le service :

```bash
sudo systemctl enable xfiles-bot.service
sudo systemctl start xfiles-bot.service
```

Pour voir les logs du bot en temps réel :
```bash
sudo journalctl -u xfiles-bot.service -f
```

---

## 🔍 Structure du Code

- [Cargo.toml](Cargo.toml) : Gère les dépendances (Teloxide, Reqwest, Serde, Tokio).
- [src/common.rs](src/common.rs) : État de la conversation, consigne système, et orchestration partagée (résumé glissant, génération de l'histoire) indépendante du provider.
- [src/gemini.rs](src/gemini.rs) : Adaptateur pour l'API REST de Gemini (génération de texte structuré JSON).
- [src/mistral.rs](src/mistral.rs) : Adaptateur pour l'API REST de Mistral.
- [src/main.rs](src/main.rs) : Contient la logique du bot Telegram, les gestionnaires de commandes, et la machine à états de dialogue.
