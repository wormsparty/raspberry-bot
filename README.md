# 🛸 Mulder & Scully - Telegram AI Dungeon Bot (Rust)

Ce projet est un exemple de bot Telegram codé en Rust qui permet de jouer à une aventure textuelle de type "AI Dungeon" en co-écriture avec un ami. L'histoire suit les agents Fox Mulder et Dana Scully dans des enquêtes humoristiques, absurdes et surréalistes (maisons marchantes, tableaux de Freud récurrents, etc.). 

Le bot utilise l'API **Gemini** pour la narration (avec un schéma JSON structuré pour garantir un comportement fiable).

---

## 🛠️ Fonctionnalités du Bot

- **Narrateur interactif** : Gemini joue le rôle de Maître de Jeu (MJ) et décrit les conséquences absurdes de vos actions.
- **Mode multijoueur (Groupes)** : Ajoutez le bot dans un groupe avec votre ami. Le bot maintient l'état de l'histoire pour le groupe entier, permettant de jouer à deux en prenant des tours.
- **Commandes intégrées** :
  - `/start [état initial]` : Démarre une nouvelle enquête absurde. Vous pouvez lui spécifier un état initial (ex : `/start Nous sommes au pôle nord, il fait froid`).
  - `/history` : Affiche l'historique complet de l'histoire générée.
  - `/help` : Affiche l'aide et les commandes.

---

## 📋 Prérequis

1. **Rust** installé sur votre machine (développement) ou sur le Raspberry Pi.
2. **Un Token de Bot Telegram** (créé via `@BotFather`).
3. **Une clé API Gemini** (obtenue sur Google AI Studio).

---

## ⚙️ Configuration

1. Clonez ou copiez ce dossier sur votre Raspberry Pi (ou machine de dev).
2. Créez un fichier nommé `.env` à la racine du projet (`/home/mob/xfiles_bot/.env`) et ajoutez vos clés :

```env
# Token Telegram (obtenu auprès de @BotFather)
TELEGRAM_BOT_TOKEN=ton_token_telegram_ici

# Clé API Gemini (obtenue sur Google AI Studio)
GEMINI_API_KEY=ta_cle_gemini_ici

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
User=mob
WorkingDirectory=/home/mob/xfiles_bot
ExecStart=/home/mob/xfiles_bot/target/release/xfiles_bot
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

- [Cargo.toml](file:///home/mob/xfiles_bot/Cargo.toml) : Gère les dépendances (Teloxide, Reqwest, Serde, Tokio).
- [src/gemini.rs](file:///home/mob/xfiles_bot/src/gemini.rs) : Contient l'intégration avec les API REST de Gemini (génération de texte structuré JSON).
- [src/main.rs](file:///home/mob/xfiles_bot/src/main.rs) : Contient la logique du bot Telegram, les gestionnaires de commandes, et la machine à états de dialogue.
