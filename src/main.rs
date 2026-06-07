mod gemini;

use std::error::Error;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, /*InputFile,*/ Message};
use teloxide::utils::command::BotCommands;
use teloxide::dispatching::dialogue::{InMemStorage, Dialogue};
use teloxide::dispatching::UpdateHandler;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub enum State {
    #[default]
    Start,
    Game {
        state: gemini::ConversationState,
    },
}

#[derive(Clone)]
pub struct Config {
    pub gemini_api_key: String,
}

type MyDialogue = Dialogue<State, InMemStorage<State>>;
type HandlerResult = Result<(), Box<dyn Error + Send + Sync>>;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Commandes de l'enquête :")]
enum Command {
    #[command(description = "Commencer une nouvelle enquête.")]
    Start,
    #[command(description = "Afficher l'aide.")]
    Help,
    #[command(description = "Afficher l'historique de l'enquête.")]
    History,
    /*#[command(description = "Générer une illustration pour la scène courante.")]
    Image,*/
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

    let gemini_api_key = std::env::var("GEMINI_API_KEY")
        .expect("GEMINI_API_KEY doit être défini dans l'environnement ou le fichier .env");

    let bot = Bot::from_env();
    let config = Config { gemini_api_key };

    let mut dispatcher = Dispatcher::builder(bot, schema())
        .dependencies(dptree::deps![InMemStorage::<State>::new(), config])
        .enable_ctrlc_handler()
        .build();

    dispatcher.dispatch().await;
}

fn schema() -> UpdateHandler<Box<dyn Error + Send + Sync + 'static>> {
    use dptree::case;

    let message_handler = Update::filter_message()
        .enter_dialogue::<Message, InMemStorage<State>, State>()
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
        Command::Start => {
            bot.send_message(msg.chat.id, "🛸 Initialisation d'une nouvelle enquête pour Mulder et Scully...").await?;
            bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;

            let mut conv_state = gemini::ConversationState::default();
            let start_msg = "Commence une nouvelle enquête de Mulder et Scully.".to_string();

            match gemini::generate_story(&config.gemini_api_key, &mut conv_state, &start_msg).await {
                Ok(story_response) => {
                    dialogue.update(State::Game { state: conv_state }).await?;

                    bot.send_message(msg.chat.id, &story_response.story_text).await?;

                    /*if story_response.should_generate_image {
                        bot.send_chat_action(msg.chat.id, ChatAction::UploadPhoto).await?;
                        log::info!("Génération de l'image de début avec le prompt: {}", story_response.image_prompt);
                        match gemini::generate_image(&config.gemini_api_key, &story_response.image_prompt).await {
                            Ok(image_bytes) => {
                                bot.send_photo(msg.chat.id, InputFile::memory(image_bytes)).await?;
                            }
                            Err(e) => {
                                log::error!("Erreur lors de la génération d'image : {}", e);
                            }
                        }
                    }*/
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
                             - Le Maître de Jeu décrira les rebondissements de l'histoire.\n\
                             - Si la situation s'y prête, une illustration style 'capture d'écran VHS' sera générée.\n\n\
                             **Commandes :**\n\
                             /start - Recommencer une nouvelle enquête\n\
                             /history - Relire le journal de l'enquête depuis le début\n\
                             /image - Générer une illustration de la scène actuelle\n\
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
                            if let Ok(story) = serde_json::from_str::<gemini::StoryResponse>(&msg_item.parts[0].text) {
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
        /*Command::Image => {
            if let Some(state) = dialogue.get().await? {
                if let State::Game { state: conv_state } = state {
                    if let Some(last_msg) = conv_state.recent.iter().rev().find(|m| m.role == "model" && !m.parts[0].text.starts_with("[Résumé")) {
                        if let Ok(story) = serde_json::from_str::<gemini::StoryResponse>(&last_msg.parts[0].text) {
                            let prompt = if !story.image_prompt.is_empty() {
                                story.image_prompt.clone()
                            } else {
                                format!(
                                    "A grainy, retro 1990s television sci-fi series VHS screenshot of a male FBI agent in a suit and a female FBI agent with red bob hair. Foggy atmosphere, dark lighting, 35mm film grain. Scene: {}",
                                    story.story_text.chars().take(200).collect::<String>()
                                )
                            };

                            bot.send_message(msg.chat.id, "🖼 Génération d'une illustration pour cette scène...").await?;
                            bot.send_chat_action(msg.chat.id, ChatAction::UploadPhoto).await?;

                            match gemini::generate_image(&config.gemini_api_key, &prompt).await {
                                Ok(image_bytes) => {
                                    bot.send_photo(msg.chat.id, InputFile::memory(image_bytes)).await?;
                                }
                                Err(e) => {
                                    log::error!("Erreur lors de la génération forcée : {}", e);
                                    bot.send_message(
                                        msg.chat.id,
                                        "❌ Impossible de générer l'illustration. Les forces occultes ont bloqué l'image !",
                                    ).await?;
                                }
                            }
                        } else {
                            bot.send_message(msg.chat.id, "Impossible de lire la scène courante.").await?;
                        }
                    } else {
                        bot.send_message(msg.chat.id, "Aucune scène à illustrer pour le moment.").await?;
                    }
                } else {
                    bot.send_message(msg.chat.id, "Aucune enquête en cours. Tapez /start pour commencer !").await?;
                }
            } else {
                bot.send_message(msg.chat.id, "Aucune enquête en cours. Tapez /start pour commencer !").await?;
            }
        }*/
    }
    Ok(())
}

async fn handle_game_state(
    bot: Bot,
    dialogue: MyDialogue,
    state: gemini::ConversationState,
    msg: Message,
    config: Config,
) -> HandlerResult {
    let text = match msg.text() {
        Some(t) => t,
        None => {
            bot.send_message(msg.chat.id, "Veuillez m'envoyer un message texte pour décrire vos actions !").await?;
            return Ok(());
        }
    };

    if text.starts_with('/') {
        return Ok(());
    }

    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;

    let mut conv_state = state;

    match gemini::generate_story(&config.gemini_api_key, &mut conv_state, text).await {
        Ok(story_response) => {
            dialogue.update(State::Game { state: conv_state }).await?;

            bot.send_message(msg.chat.id, &story_response.story_text).await?;
/*
            if story_response.should_generate_image {
                bot.send_chat_action(msg.chat.id, ChatAction::UploadPhoto).await?;
                log::info!("Génération d'une image avec le prompt: {}", story_response.image_prompt);
                match gemini::generate_image(&config.gemini_api_key, &story_response.image_prompt).await {
                    Ok(image_bytes) => {
                        bot.send_photo(msg.chat.id, InputFile::memory(image_bytes)).await?;
                    }
                    Err(e) => {
                        log::error!("Erreur lors de la génération d'image : {}", e);
                    }
                }
            }*/
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
