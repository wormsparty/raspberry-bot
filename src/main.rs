mod common;
mod gemini;
mod mistral;

use std::error::Error;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use teloxide::dispatching::dialogue::{Dialogue, Storage};
use teloxide::dispatching::UpdateHandler;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatId, /*InputFile,*/ Message};
use teloxide::utils::command::BotCommands;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub enum State {
    #[default]
    Start,
    Game {
        state: common::ConversationState,
    },
}

#[derive(Clone)]
pub struct Config {
    // Pour utiliser Gemini à nouveau, décommentez ceci et commentez mistral_api_key
    pub gemini_api_key: String,
    //pub mistral_api_key: String,
}

#[derive(Clone)]
pub struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
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
            if !path.exists() {
                return Ok(None);
            }
            let content = tokio::fs::read(&path).await?;
            let dialogue = serde_json::from_slice(&content)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(Some(dialogue))
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
            tokio::fs::write(&path, content).await?;
            Ok(())
        })
    }

    fn remove_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'static>> {
        let path = self.path(chat_id);
        Box::pin(async move {
            if path.exists() {
                tokio::fs::remove_file(&path).await?;
            }
            Ok(())
        })
    }
}

type MyDialogue = Dialogue<State, FileStorage>;
type HandlerResult = Result<(), Box<dyn Error + Send + Sync>>;

#[derive(BotCommands, Clone, Debug, PartialEq)]
#[command(rename_rule = "lowercase", description = "Commandes de l'enquête :")]
enum Command {
    #[command(description = "Commencer une nouvelle enquête.")]
    Start(String),
    #[command(description = "Réinitialiser l'enquête.")]
    Restart(String),
    #[command(description = "Afficher l'aide.")]
    Help,
    #[command(description = "Afficher l'historique de l'enquête.")]
    History,
    #[command(description = "Obtenir un résumé complet de l'histoire pour la reprendre ailleurs.")]
    Summary,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init();
    log::info!("Démarrage du bot X-Files...");

    if std::env::var("TELOXIDE_TOKEN").is_err() {
        if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
            // SAFETY: single-threaded initialization before tokio runtime starts
            unsafe { std::env::set_var("TELOXIDE_TOKEN", token) };
        }
    }

    // --- Configuration de l'API Key ---
    // Gemini (Commenté pour utiliser Mistral)
    let gemini_api_key = std::env::var("GEMINI_API_KEY")
        .expect("GEMINI_API_KEY doit être défini dans l'environnement ou le fichier .env");

    // Mistral
    //let mistral_api_key = std::env::var("MISTRAL_API_KEY")
    //    .expect("MISTRAL_API_KEY doit être défini dans l'environnement ou le fichier .env");

    let bot = Bot::from_env();
    let config = Config {
        gemini_api_key,
        //mistral_api_key,
    };

    let mut dispatcher = Dispatcher::builder(bot, schema())
        .dependencies(dptree::deps![Arc::new(FileStorage::new(std::path::PathBuf::from("sessions"))), config])
        .enable_ctrlc_handler()
        .build();

    dispatcher.dispatch().await;
}

fn schema() -> UpdateHandler<Box<dyn Error + Send + Sync + 'static>> {
    use dptree::case;

    let message_handler = Update::filter_message()
        .chain(dptree::filter(|msg: Message| {
            if let Some(text) = msg.text() {
                !text.starts_with("/ignore")
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
        "🛸 Aucune enquête n'est en cours.\n\nTapez /start pour commencer une aventure avec Mulder et Scully !",
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
        Command::Start(initial_state) | Command::Restart(initial_state) => {
            bot.send_message(msg.chat.id, "🛸 Initialisation d'une nouvelle enquête pour Mulder et Scully...").await?;
            bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;

            let mut conv_state = common::ConversationState::default();
            let start_msg = if initial_state.trim().is_empty() {
                "Commence une nouvelle enquête de Mulder et Scully.".to_string()
            } else {
                format!(
                    "Commence une nouvelle enquête de Mulder et Scully. État initial : {}",
                    initial_state.trim()
                )
            };

            // --- Choix du modèle LLM ---
            // Utilisation de Gemini (Commenté) :
            match gemini::generate_story(&config.gemini_api_key, &mut conv_state, &start_msg).await {
            
            // Utilisation de Mistral :
            //match mistral::generate_story(&config.mistral_api_key, &mut conv_state, &start_msg).await {
                Ok(story_response) => {
                    dialogue.update(State::Game { state: conv_state }).await?;

                    bot.send_message(msg.chat.id, &story_response.story_text).await?;
                }
                Err(e) => {
                    log::error!("Erreur lors du démarrage du jeu : {}", e);
                    bot.send_message(
                        msg.chat.id,
                        "👽 Les ondes cosmiques perturbent l'API (impossible de démarrer l'enquête). Réessayez !",
                    ).await?;
                }
            }
        }
        Command::Help => {
            let help_text = "🕵️‍♂️ **Bienvenue dans l'X-Files Generator !** 🕵️‍♀️\n\n\
                             Vous co-écrivez une enquête avec Mulder et Scully.\n\n\
                             **Comment jouer :**\n\
                             - Écrivez simplement ce que font ou disent nos deux agents (ex: 'Mulder fouille la poubelle').\n\
                             - Le Maître de Jeu décrira les rebondissements de l'histoire.\n\n\
                             **Commandes :**\n\
                             /start [état] - Commencer une nouvelle enquête avec un état initial facultatif\n\
                             /restart [état] - Réinitialiser l'enquête avec un état initial facultatif\n\
                             /summary - Obtenir un résumé complet de l'histoire pour la reprendre ailleurs\n\
                             /history - Relire le journal de l'enquête depuis le début\n\
                             /help - Afficher ce message d'aide";
            bot.send_message(msg.chat.id, help_text).await?;
        }
        Command::History => {
            if let Some(state) = dialogue.get().await? {
                if let State::Game { state: conv_state } = state {
                    let mut chronicle = String::from("📖 **Journal de l'enquête :**\n\n");

                    if !conv_state.summary.is_empty() {
                        chronicle.push_str(&format!("📋 _Résumé des événements passés : {}_\n\n---\n\n", conv_state.summary));
                    }

                    let mut has_entries = false;
                    for msg_item in &conv_state.recent {
                        if msg_item.role == "user" {
                            if msg_item.parts[0].text == "Commence une nouvelle enquête de Mulder et Scully." {
                                continue;
                            }
                            chronicle.push_str(&format!("👉 _Action : {}_\n\n", msg_item.parts[0].text));
                            has_entries = true;
                        } else if msg_item.role == "model" {
                            // Ignorer le message de résumé injecté (commence par "[Résumé")
                            if msg_item.parts[0].text.starts_with("[Résumé") {
                                continue;
                            }
                            if let Ok(story) = serde_json::from_str::<common::StoryResponse>(&msg_item.parts[0].text) {
                                chronicle.push_str(&format!("{}\n\n", story.story_text));
                            } else {
                                chronicle.push_str(&format!("{}\n\n", msg_item.parts[0].text));
                            }
                            has_entries = true;
                        }
                    }

                    if has_entries || !conv_state.summary.is_empty() {
                        bot.send_message(msg.chat.id, chronicle).await?;
                    } else {
                        bot.send_message(msg.chat.id, "L'histoire commence à peine. Envoyez votre première action !").await?;
                    }
                } else {
                    bot.send_message(msg.chat.id, "Aucune enquête en cours. Tapez /start pour commencer !").await?;
                }
            } else {
                bot.send_message(msg.chat.id, "Aucune enquête en cours. Tapez /start pour commencer !").await?;
            }
        }
        Command::Summary => {
            if let Some(state) = dialogue.get().await? {
                if let State::Game { state: conv_state } = state {
                    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;
                    
                    // --- Choix du modèle LLM ---
                    // Utilisation de Gemini (Commenté) :
                    match gemini::get_story_summary(&config.gemini_api_key, &conv_state).await {
                    
                    // Utilisation de Mistral :
                    //match mistral::get_story_summary(&config.mistral_api_key, &conv_state).await {
                        Ok(summary) => {
                            let reply = format!(
                                "📋 **Résumé de l'enquête actuelle (prêt à être copié pour /start ou /restart) :**\n\n```\n{}\n```",
                                summary
                            );
                            bot.send_message(msg.chat.id, reply).await?;
                        }
                        Err(e) => {
                            log::error!("Erreur lors de la génération du résumé : {}", e);
                            bot.send_message(
                                msg.chat.id,
                                "👽 Impossible de générer le résumé. Réessayez !",
                            ).await?;
                        }
                    }
                } else {
                    bot.send_message(msg.chat.id, "Aucune enquête en cours. Tapez /start pour commencer !").await?;
                }
            } else {
                bot.send_message(msg.chat.id, "Aucune enquête en cours. Tapez /start pour commencer !").await?;
            }
        }
    }
    Ok(())
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

    // --- Choix du modèle LLM ---
    // Utilisation de Gemini (Commenté) :
    match gemini::generate_story(&config.gemini_api_key, &mut conv_state, text).await {

    // Utilisation de Mistral :
    //match mistral::generate_story(&config.mistral_api_key, &mut conv_state, text).await {
        Ok(story_response) => {
            dialogue.update(State::Game { state: conv_state }).await?;

            bot.send_message(msg.chat.id, &story_response.story_text).await?;
        }
        Err(e) => {
            log::error!("Erreur lors de la génération de l'histoire : {}", e);
            bot.send_message(
                msg.chat.id,
                "👽 L'espace-temps s'est plié anormalement. Veuillez reformuler ou réécrire votre dernière action !",
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

        let cmd = Command::parse("/restart", "bot").unwrap();
        assert_eq!(cmd, Command::Restart("".to_string()));

        let cmd = Command::parse("/restart Nous sommes au pôle nord, il fait froid", "bot").unwrap();
        assert_eq!(cmd, Command::Restart("Nous sommes au pôle nord, il fait froid".to_string()));

        let cmd = Command::parse("/summary", "bot").unwrap();
        assert_eq!(cmd, Command::Summary);
    }
}

