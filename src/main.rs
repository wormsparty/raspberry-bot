mod common;
mod gemini;
mod image;
mod mistral;

use std::error::Error;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use teloxide::dispatching::dialogue::{Dialogue, Storage};
use teloxide::dispatching::UpdateHandler;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatId, InputFile, Message};
use teloxide::utils::command::BotCommands;

use common::{Config, ModelProvider};

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub enum State {
    #[default]
    Start,
    Game {
        state: common::ConversationState,
    },
}

#[derive(Clone)]
pub struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("Impossible de créer le dossier de sessions {:?} : {}", dir, e));
        Self { dir }
    }

    fn path(&self, chat_id: ChatId) -> PathBuf {
        self.dir.join(format!("{}.json", chat_id))
    }
}

impl<D> Storage<D> for FileStorage
where
    D: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
{
    type Error = std::io::Error;

    fn get_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<D>, Self::Error>> + Send + 'static>> {
        let path = self.path(chat_id);
        Box::pin(async move {
            let content = match tokio::fs::read(&path).await {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            match serde_json::from_slice(&content) {
                Ok(dialogue) => Ok(Some(dialogue)),
                Err(e) => {
                    // Session corrompue : on repart de zéro plutôt que de bloquer le chat
                    log::warn!("Session illisible ({}), réinitialisation : {}", path.display(), e);
                    Ok(None)
                }
            }
        })
    }

    fn update_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
        dialogue: D,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'static>> {
        let path = self.path(chat_id);
        Box::pin(async move {
            let content = serde_json::to_vec(&dialogue)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            // Écriture atomique : fichier temporaire puis rename, pour ne jamais
            // laisser un JSON tronqué si le process meurt en pleine écriture
            let tmp_path = path.with_extension("json.tmp");
            tokio::fs::write(&tmp_path, content).await?;
            tokio::fs::rename(&tmp_path, &path).await?;
            Ok(())
        })
    }

    fn remove_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'static>> {
        let path = self.path(chat_id);
        Box::pin(async move {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        })
    }
}

type MyDialogue = Dialogue<State, FileStorage>;
type HandlerResult = Result<(), Box<dyn Error + Send + Sync>>;

#[derive(BotCommands, Clone, Debug, PartialEq)]
#[command(rename_rule = "lowercase", description = "Commandes de la mission :")]
enum Command {
    #[command(description = "Commencer une nouvelle mission.")]
    Start(String),
    #[command(description = "Afficher l'aide.")]
    Help,
    #[command(description = "Obtenir un résumé complet de la mission pour la reprendre ailleurs.")]
    Summary,
    #[command(description = "Choisir le modèle IA : /model gemini ou /model mistral.")]
    Model(String),
    #[command(description = "Activer ou désactiver la génération d'images : /image on ou /image off.")]
    Image(String),
    #[command(description = "Déployer la dernière version du bot (admin seulement). Ajouter -y pour ignorer les modifications locales.")]
    Deploy(String),
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init();
    log::info!("Démarrage du bot Star Trek...");

    let token = std::env::var("TELOXIDE_TOKEN")
        .or_else(|_| std::env::var("TELEGRAM_BOT_TOKEN"))
        .expect("TELOXIDE_TOKEN ou TELEGRAM_BOT_TOKEN doit être défini dans l'environnement ou le fichier .env");
    let bot = Bot::new(token);

    // --- Configuration du Modèle et des API Keys ---
    let provider_str = std::env::var("MODEL_PROVIDER")
        .unwrap_or_else(|_| "gemini".to_string())
        .to_lowercase();

    let default_provider = match provider_str.as_str() {
        "gemini" => ModelProvider::Gemini,
        "mistral" => ModelProvider::Mistral,
        other => {
            panic!("MODEL_PROVIDER inconnu : '{}'. Les valeurs possibles sont 'gemini' ou 'mistral'.", other);
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("Impossible de construire le client HTTP");

    let config = Config {
        client,
        default_provider,
        gemini_key: std::env::var("GEMINI_API_KEY").ok(),
        mistral_key: std::env::var("MISTRAL_API_KEY").ok(),
        openrouter_key: std::env::var("OPENROUTER_API_KEY").ok(),
    };

    // La clé du provider par défaut est indispensable ; les autres sont optionnelles.
    if let Err(e) = config.key_for(default_provider) {
        panic!("{}", e);
    }

    // Valider ADMIN_USER_ID au démarrage pour détecter les fautes de frappe immédiatement.
    if let Ok(raw) = std::env::var("ADMIN_USER_ID") {
        if raw.parse::<u64>().is_err() {
            panic!("ADMIN_USER_ID='{}' n'est pas un entier u64 valide — corrigez le fichier .env", raw);
        }
    }

    let mut dispatcher = Dispatcher::builder(bot, schema())
        .dependencies(dptree::deps![Arc::new(FileStorage::new(std::path::PathBuf::from("sessions"))), config])
        .enable_ctrlc_handler()
        .build();

    dispatcher.dispatch().await;
}

static HOLODECK_ERRORS: &[&str] = &[
    "⚠️ *Perturbation ionique détectée.* Les capteurs de visualisation du pont sont temporairement hors ligne. Continuez la mission, Commandant.",
    "⚠️ *Holodeck en maintenance d'urgence.* La projection visuelle de la scène ne peut être affichée pour le moment. L'ingénieur en chef s'en occupe.",
    "⚠️ *Communications interstellaires dégradées.* Les satellites d'imagerie subspaciale ne répondent plus. La mission continue sans relais visuel.",
    "⚠️ *Matrice holoémettrice surchargée.* Le système de visualisation a besoin de 47 secondes pour se recalibrer. Restez en alerte.",
    "⚠️ *Quota de transfert subspacial atteint.* La bande passante de la communication visuelle est saturée. Reprise dans le prochain quadrant.",
];

const HOLODECK_BUDGET_ERROR: &str = "⚠️ *Alerte du Département des Finances de la Flotte Stellaire !* \
    Nos réserves de latinum numérique sont épuisées — le Quartier Général a suspendu les transmissions holoémetteurs. \
    La Flotte doit recharger ses crédits de communication subspaciale avant de pouvoir reprendre les projections visuelles. \
    La mission continue sans relais visuel pour le moment, Commandant.";

fn holodeck_error_message(err: &dyn std::fmt::Display) -> &'static str {
    let err_str = err.to_string();
    log::warn!("Erreur génération image : {}", err_str);
    if err_str.contains("402") {
        return HOLODECK_BUDGET_ERROR;
    }
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % HOLODECK_ERRORS.len();
    HOLODECK_ERRORS[idx]
}

async fn maybe_send_image(
    bot: &Bot,
    chat_id: ChatId,
    config: &Config,
    image_enabled: bool,
    scene_description: &str,
) {
    if !image_enabled || scene_description.is_empty() {
        return;
    }
    let key = match &config.openrouter_key {
        Some(k) => k.clone(),
        None => return,
    };

    if let Err(e) = bot.send_chat_action(chat_id, ChatAction::UploadPhoto).await {
        log::warn!("Impossible d'envoyer l'action upload_photo : {}", e);
    }

    match image::generate_scene_image(&config.client, &key, scene_description).await {
        Ok(bytes) => {
            if let Err(e) = bot.send_photo(chat_id, InputFile::memory(bytes)).await {
                log::error!("Impossible d'envoyer la photo Telegram : {}", e);
            }
        }
        Err(e) => {
            let msg = holodeck_error_message(e.as_ref());
            if let Err(send_err) = bot.send_message(chat_id, msg).await {
                log::error!("Impossible d'envoyer le message d'erreur holodeck : {}", send_err);
            }
        }
    }
}

fn schema() -> UpdateHandler<Box<dyn Error + Send + Sync + 'static>> {
    use dptree::case;

    let message_handler = Update::filter_message()
        .chain(dptree::filter(|msg: Message| {
            // Un message qui répond à un autre message du chat (un joueur qui
            // s'adresse à un autre joueur) n'est pas une action de jeu.
            if msg.reply_to_message().is_some() {
                return false;
            }
            if let Some(text) = msg.text() {
                // /ignore permet de parler aux autres joueurs du chat sans que
                // le message ne soit interprété comme une action du jeu
                !text.starts_with("/ignore") && !text.starts_with("/i ")
            } else {
                true
            }
        }))
        .enter_dialogue::<Message, FileStorage, State>()
        .branch(
            teloxide::filter_command::<Command, _>()
                .endpoint(handle_command),
        )
        .branch(case![State::Start].endpoint(handle_start_state))
        .branch(case![State::Game { state }].endpoint(handle_game_state));

    message_handler
}

async fn handle_start_state(bot: Bot, msg: Message) -> HandlerResult {
    bot.send_message(
        msg.chat.id,
        "🖖 Aucune mission n'est en cours.\n\nTapez /start pour commencer une aventure dans l'univers Star Trek !",
    )
    .await?;
    Ok(())
}

async fn handle_command(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    cmd: Command,
    config: Config,
) -> HandlerResult {
    match cmd {
        Command::Start(initial_state) => {
            bot.send_message(msg.chat.id, "🖖 Initialisation d'une nouvelle mission dans l'univers Star Trek...").await?;
            bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;

            // Conserver le modèle et les préférences d'image de la session précédente
            let (previous_provider, previous_image_enabled) = match dialogue.get().await? {
                Some(State::Game { state }) => (state.provider, state.image_enabled),
                _ => (None, true),
            };
            let mut conv_state = common::ConversationState {
                provider: previous_provider,
                image_enabled: previous_image_enabled,
                ..Default::default()
            };

            let start_msg = if initial_state.trim().is_empty() {
                "Commence une nouvelle aventure dans l'univers Star Trek.".to_string()
            } else {
                format!(
                    "Commence une nouvelle aventure dans l'univers Star Trek. État initial : {}",
                    initial_state.trim()
                )
            };

            match common::generate_story(&config, &mut conv_state, &start_msg).await {
                Ok(story_response) => {
                    let image_enabled = conv_state.image_enabled;
                    dialogue.update(State::Game { state: conv_state }).await?;
                    bot.send_message(msg.chat.id, &story_response.story_text).await?;
                    maybe_send_image(&bot, msg.chat.id, &config, image_enabled, &story_response.scene_description).await;
                }
                Err(e) => {
                    log::error!("Erreur lors du démarrage du jeu : {}", e);
                    bot.send_message(
                        msg.chat.id,
                        "🖖 Les communications subspaciales sont perturbées (impossible de démarrer la mission). Réessayez !",
                    ).await?;
                }
            }
        }
        Command::Help => {
            let help_text = "🖖 Bienvenue dans le Star Trek Adventure Generator ! 🖖\n\n\
                             Vous vivez une aventure interactive dans l'univers de Star Trek.\n\n\
                             Comment jouer :\n\
                             - Tapez /start pour lancer une nouvelle mission. Le Narrateur vous demandera votre grade, le nom de votre vaisseau et l'époque choisie.\n\
                             - Décrivez ensuite vos actions librement ou choisissez parmi les options proposées.\n\
                             - Le Narrateur gère un système de dés (d20) pour les actions à enjeu : combats, négociations, manœuvres critiques...\n\
                             - Préfixez un message par /ignore (ou /i) pour parler aux autres joueurs sans que le bot ne réagisse.\n\n\
                             Commandes :\n\
                             /start [état] - Commencer une nouvelle mission avec un état initial facultatif\n\
                             /summary - Obtenir un résumé complet de la mission pour la reprendre ailleurs\n\
                             /model gemini|mistral - Choisir le modèle IA pour la suite de la mission\n\
                             /image on|off - Activer ou désactiver la génération d'images\n\
                             /deploy [-y] - Déployer la dernière version (admin) ; -y pour ignorer les modifications locales\n\
                             /help - Afficher ce message d'aide";
            bot.send_message(msg.chat.id, help_text).await?;
        }
        Command::Summary => {
            if let Some(State::Game { state: conv_state }) = dialogue.get().await? {
                bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;

                match common::get_story_summary(&config, &conv_state).await {
                    Ok(summary) => {
                        let reply = format!(
                            "📋 Résumé de la mission actuelle (copiez-le comme état initial de /start) :\n\n{}",
                            summary
                        );
                        bot.send_message(msg.chat.id, reply).await?;
                    }
                    Err(e) => {
                        log::error!("Erreur lors de la génération du résumé : {}", e);
                        bot.send_message(
                            msg.chat.id,
                            "🖖 Impossible de générer le résumé. Réessayez !",
                        ).await?;
                    }
                }
            } else {
                bot.send_message(msg.chat.id, "Aucune mission en cours. Tapez /start pour commencer !").await?;
            }
        }
        Command::Model(model_name) => {
            let model_word = model_name.split_whitespace().next().unwrap_or("").to_lowercase();
            let provider = match model_word.as_str() {
                "gemini" => ModelProvider::Gemini,
                "mistral" => ModelProvider::Mistral,
                _ => {
                    bot.send_message(msg.chat.id, "⚠️ Modèle inconnu. Utilisez /model gemini ou /model mistral.").await?;
                    return Ok(());
                }
            };

            if let Err(e) = config.key_for(provider) {
                bot.send_message(msg.chat.id, format!("⚠️ {}", e)).await?;
            } else if let Some(State::Game { state: mut conv_state }) = dialogue.get().await? {
                conv_state.provider = Some(provider);
                dialogue.update(State::Game { state: conv_state }).await?;
                bot.send_message(
                    msg.chat.id,
                    format!("🖖 Modèle changé : la suite de la mission sera générée par {}.", provider),
                ).await?;
            } else {
                bot.send_message(
                    msg.chat.id,
                    "Aucune mission en cours. Lancez /start, puis choisissez le modèle avec /model gemini ou /model mistral.",
                ).await?;
            }
        }
        Command::Image(arg) => {
            let enabled = match arg.trim().to_lowercase().as_str() {
                "on" => true,
                "off" => false,
                _ => {
                    bot.send_message(msg.chat.id, "⚠️ Utilisez /image on ou /image off.").await?;
                    return Ok(());
                }
            };

            if let Some(State::Game { state: mut conv_state }) = dialogue.get().await? {
                conv_state.image_enabled = enabled;
                dialogue.update(State::Game { state: conv_state }).await?;
                let status = if enabled { "activée" } else { "désactivée" };
                bot.send_message(msg.chat.id, format!("🖖 Génération d'images {} pour cette mission.", status)).await?;
            } else {
                bot.send_message(
                    msg.chat.id,
                    "Aucune mission en cours. Lancez /start, puis utilisez /image on ou /image off.",
                ).await?;
            }
        }
        Command::Deploy(args) => {
            handle_deploy(&bot, &msg, args.trim()).await?;
        }
    }
    Ok(())
}

async fn handle_deploy(bot: &Bot, msg: &Message, args: &str) -> HandlerResult {
    // Vérification admin
    let admin_id = std::env::var("ADMIN_USER_ID")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    let requester_id = msg.from.as_ref().map(|u| u.id.0);

    match (admin_id, requester_id) {
        (None, _) => {
            bot.send_message(
                msg.chat.id,
                "⚠️ ADMIN_USER_ID n'est pas configuré dans le .env — commande désactivée.",
            )
            .await?;
            return Ok(());
        }
        (Some(admin), Some(req)) if admin == req => {}
        _ => {
            bot.send_message(msg.chat.id, "⛔ Accès refusé.").await?;
            return Ok(());
        }
    }

    let force = args == "-y";

    // Étape 1 : vérifier l'état du dépôt (staged + unstaged)
    if !force {
        let status = match tokio::process::Command::new("/usr/bin/git")
            .args(["status", "--porcelain"])
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                bot.send_message(msg.chat.id, format!("❌ Impossible de lancer git : {}", e))
                    .await?;
                return Ok(());
            }
        };

        if !status.stdout.trim_ascii().is_empty() {
            let stat = String::from_utf8_lossy(&status.stdout);
            bot.send_message(
                msg.chat.id,
                format!(
                    "⚠️ Des modifications locales existent (non commitées ou stagées) :\n\n{}\n\nUtilisez /deploy -y pour déployer quand même.",
                    stat.trim()
                ),
            )
            .await?;
            return Ok(());
        }
    }

    // Étape 2 : git pull
    bot.send_message(msg.chat.id, "🔄 git pull en cours...").await?;
    let pull = match tokio::process::Command::new("/usr/bin/git")
        .args(["pull"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            bot.send_message(msg.chat.id, format!("❌ Impossible de lancer git pull : {}", e))
                .await?;
            return Ok(());
        }
    };

    if !pull.status.success() {
        let err = String::from_utf8_lossy(&pull.stderr);
        bot.send_message(
            msg.chat.id,
            format!("❌ git pull a échoué :\n\n{}", truncate_head(&err, 3800)),
        )
        .await?;
        return Ok(());
    }
    let pull_out = String::from_utf8_lossy(&pull.stdout);

    // Étape 3 : cargo build --release
    bot.send_message(
        msg.chat.id,
        format!(
            "🔨 Compilation en cours (peut prendre plusieurs minutes)...\n{}",
            pull_out.trim()
        ),
    )
    .await?;

    let build = match tokio::process::Command::new("/home/mob/.cargo/bin/cargo")
        .args(["build", "--release"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            bot.send_message(
                msg.chat.id,
                format!("❌ Impossible de lancer cargo : {}\n\nVérifiez que le chemin `/home/mob/.cargo/bin/cargo` est correct.", e),
            )
            .await?;
            return Ok(());
        }
    };

    if !build.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        bot.send_message(
            msg.chat.id,
            format!(
                "❌ Compilation échouée — le service n'a PAS été redémarré, l'ancienne version continue de tourner.\n\n{}",
                truncate_tail(&stderr, 3600)
            ),
        )
        .await?;
        return Ok(());
    }

    // Étape 4 : redémarrage
    bot.send_message(
        msg.chat.id,
        "✅ Compilation réussie ! Redémarrage du service dans 1 seconde...",
    )
    .await?;

    // On spawn pour laisser le temps à Telegram de recevoir le message avant que le process meure.
    // Si le restart échoue (process toujours vivant), on notifie l'admin.
    let bot_clone = bot.clone();
    let chat_id = msg.chat.id;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        match tokio::process::Command::new("/usr/bin/sudo")
            .args(["systemctl", "restart", "xfiles-bot"])
            .output()
            .await
        {
            Err(e) => {
                log::error!("Impossible de lancer sudo systemctl restart : {}", e);
                let _ = bot_clone.send_message(chat_id, format!("❌ Redémarrage impossible : {}", e)).await;
            }
            Ok(out) if !out.status.success() => {
                let err = String::from_utf8_lossy(&out.stderr);
                log::error!("systemctl restart a échoué : {}", err);
                let _ = bot_clone
                    .send_message(chat_id, format!("❌ Redémarrage échoué :\n{}", truncate_tail(&err, 1000)))
                    .await;
            }
            Ok(_) => {} // succès → le process meurt, rien à envoyer
        }
    });

    Ok(())
}

fn truncate_tail(s: &str, max_chars: usize) -> String {
    let indices: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    if indices.len() <= max_chars {
        return s.to_string();
    }
    format!("[...]\n{}", &s[indices[indices.len() - max_chars]..])
}

fn truncate_head(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        None => s.to_string(),
        Some((end, _)) => format!("{}\n[...]", &s[..end]),
    }
}

async fn handle_game_state(
    bot: Bot,
    dialogue: MyDialogue,
    state: common::ConversationState,
    msg: Message,
    config: Config,
) -> HandlerResult {
    let text = match msg.text() {
        Some(t) => t,
        None => {
            if msg.photo().is_some() || msg.animation().is_some() {
                return Ok(());
            }
            if let Some(doc) = msg.document() {
                if let Some(mime) = &doc.mime_type {
                    if mime.as_ref().starts_with("image/") {
                        return Ok(());
                    }
                }
            }
            bot.send_message(msg.chat.id, "Veuillez m'envoyer un message texte pour décrire vos actions !").await?;
            return Ok(());
        }
    };

    if text.starts_with('/') {
        return Ok(());
    }

    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;

    let mut conv_state = state;

    match common::generate_story(&config, &mut conv_state, text).await {
        Ok(story_response) => {
            let image_enabled = conv_state.image_enabled;
            dialogue.update(State::Game { state: conv_state }).await?;
            bot.send_message(msg.chat.id, &story_response.story_text).await?;
            maybe_send_image(&bot, msg.chat.id, &config, image_enabled, &story_response.scene_description).await;
        }
        Err(e) => {
            log::error!("Erreur lors de la génération de l'histoire : {}", e);
            bot.send_message(
                msg.chat.id,
                "🖖 Les communications subspaciales sont interrompues. Veuillez reformuler ou réécrire votre dernière action !",
            ).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_parsing() {
        let cmd = Command::parse("/start", "bot").unwrap();
        assert_eq!(cmd, Command::Start("".to_string()));

        let cmd = Command::parse("/start Nous sommes au pôle nord, il fait froid", "bot").unwrap();
        assert_eq!(cmd, Command::Start("Nous sommes au pôle nord, il fait froid".to_string()));

        let cmd = Command::parse("/summary", "bot").unwrap();
        assert_eq!(cmd, Command::Summary);

        let cmd = Command::parse("/model gemini", "bot").unwrap();
        assert_eq!(cmd, Command::Model("gemini".to_string()));

        let cmd = Command::parse("/model mistral", "bot").unwrap();
        assert_eq!(cmd, Command::Model("mistral".to_string()));
    }
}
