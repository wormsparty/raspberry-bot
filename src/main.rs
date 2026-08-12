mod common;
mod gemini;
mod image;
mod mistral;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serenity::all::*;
use serenity::async_trait;
use serenity::Client as SerenityClient;
use tokio::sync::Mutex;

use common::{Config, ConversationState, ModelProvider};

// Limite de taille d'un message Discord.
const DISCORD_MSG_LIMIT: usize = 2000;

// Nom du service systemd, surchargeable pour les installations non standard.
fn service_name() -> String {
    std::env::var("SERVICE_NAME").unwrap_or_else(|_| "raspberry-bot".to_string())
}

// Chemin vers cargo : le PATH d'un service systemd ne contient pas ~/.cargo/bin,
// on résout donc explicitement sans coder en dur le home d'un utilisateur.
fn cargo_path() -> OsString {
    if let Some(path) = std::env::var_os("CARGO_BIN") {
        return path;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".cargo").join("bin").join("cargo");
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }
    // Dernier recours : résolution via le PATH du processus.
    OsString::from("cargo")
}

// --- Persistance des sessions ------------------------------------------------

// Une session par salon Discord : tous les joueurs d'un même salon partagent
// la même aventure, comme dans un groupe.
#[derive(Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("Impossible de créer le dossier de sessions {:?} : {}", dir, e));
        Self { dir }
    }

    fn path(&self, channel_id: ChannelId) -> PathBuf {
        self.dir.join(format!("{}.json", channel_id.get()))
    }

    async fn load(&self, channel_id: ChannelId) -> Option<ConversationState> {
        let path = self.path(channel_id);
        let content = match tokio::fs::read(&path).await {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::error!("Lecture de session impossible ({}) : {}", path.display(), e);
                return None;
            }
        };
        match serde_json::from_slice(&content) {
            Ok(state) => Some(state),
            Err(e) => {
                // Session corrompue : on repart de zéro plutôt que de bloquer le salon
                log::warn!("Session illisible ({}), réinitialisation : {}", path.display(), e);
                None
            }
        }
    }

    async fn save(&self, channel_id: ChannelId, state: &ConversationState) -> std::io::Result<()> {
        let path = self.path(channel_id);
        let content = serde_json::to_vec(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Écriture atomique : fichier temporaire puis rename, pour ne jamais
        // laisser un JSON tronqué si le process meurt en pleine écriture
        let tmp_path = path.with_extension("json.tmp");
        tokio::fs::write(&tmp_path, content).await?;
        tokio::fs::rename(&tmp_path, &path).await?;
        Ok(())
    }
}

// --- Envoi des réponses ------------------------------------------------------

// Le bot répond soit à une slash command (réponse différée), soit à un message
// normal du salon. Cette enum unifie les deux chemins d'envoi.
enum Reply<'a> {
    Channel(&'a Context, ChannelId),
    Command(&'a Context, &'a CommandInteraction),
}

impl Reply<'_> {
    fn ctx(&self) -> &Context {
        match self {
            Reply::Channel(ctx, _) => ctx,
            Reply::Command(ctx, _) => ctx,
        }
    }

    fn channel_id(&self) -> ChannelId {
        match self {
            Reply::Channel(_, id) => *id,
            Reply::Command(_, cmd) => cmd.channel_id,
        }
    }

    async fn typing(&self) {
        if let Err(e) = self.channel_id().broadcast_typing(&self.ctx().http).await {
            log::warn!("Impossible d'envoyer l'indicateur de saisie : {}", e);
        }
    }

    // Envoie un texte, découpé en plusieurs messages si nécessaire.
    async fn text(&self, content: &str) {
        let mut chunks = split_message(content, DISCORD_MSG_LIMIT);
        if chunks.is_empty() {
            log::warn!("Réponse vide : rien à envoyer.");
            // Une interaction différée doit recevoir au moins une réponse,
            // sinon Discord affiche « réflexion… » indéfiniment.
            match self {
                Reply::Command(..) => chunks.push("🧛 …".to_string()),
                Reply::Channel(..) => return,
            }
        }
        for chunk in chunks {
            let result = match self {
                Reply::Channel(ctx, id) => id.say(&ctx.http, &chunk).await.map(|_| ()),
                Reply::Command(ctx, cmd) => cmd
                    .create_followup(
                        &ctx.http,
                        CreateInteractionResponseFollowup::new().content(&chunk),
                    )
                    .await
                    .map(|_| ()),
            };
            if let Err(e) = result {
                log::error!("Impossible d'envoyer le message Discord : {}", e);
                return;
            }
        }
    }

    async fn image(&self, bytes: Vec<u8>) {
        let attachment = CreateAttachment::bytes(bytes, "scene.png");
        let result = match self {
            Reply::Channel(ctx, id) => id
                .send_message(&ctx.http, CreateMessage::new().add_file(attachment))
                .await
                .map(|_| ()),
            Reply::Command(ctx, cmd) => cmd
                .create_followup(
                    &ctx.http,
                    CreateInteractionResponseFollowup::new().add_file(attachment),
                )
                .await
                .map(|_| ()),
        };
        if let Err(e) = result {
            log::error!("Impossible d'envoyer l'image Discord : {}", e);
        }
    }
}

// Découpe un texte en morceaux d'au plus `limit` caractères, en préservant
// autant que possible les fins de ligne.
fn split_message(text: &str, limit: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.split_inclusive('\n') {
        let mut line = line;
        // Une ligne à elle seule trop longue est coupée brutalement.
        while line.chars().count() > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let (head, tail) = split_at_chars(line, limit);
            chunks.push(head.to_string());
            line = tail;
        }
        if current.chars().count() + line.chars().count() > limit {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    chunks.push(current);

    chunks
        .into_iter()
        .map(|c| c.trim_end().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

fn split_at_chars(s: &str, n: usize) -> (&str, &str) {
    match s.char_indices().nth(n) {
        Some((idx, _)) => s.split_at(idx),
        None => (s, ""),
    }
}

// --- Bot ---------------------------------------------------------------------

struct Handler {
    config: Config,
    store: SessionStore,
    // Un verrou par salon : deux joueurs qui écrivent en même temps ne doivent
    // pas écraser mutuellement l'état de l'aventure.
    locks: Mutex<HashMap<ChannelId, Arc<Mutex<()>>>>,
}

impl Handler {
    async fn channel_lock(&self, channel_id: ChannelId) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(channel_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn maybe_send_image(&self, reply: &Reply<'_>, image_enabled: bool, scene_description: &str) {
        if !image_enabled || scene_description.is_empty() {
            return;
        }
        let key = match &self.config.openrouter_key {
            Some(k) => k.clone(),
            None => return,
        };

        reply.typing().await;

        match image::generate_scene_image(&self.config.client, &key, scene_description).await {
            Ok(bytes) => reply.image(bytes).await,
            Err(e) => reply.text(vision_error_message(e.as_ref())).await,
        }
    }

    // Traite une action de jeu (texte libre) pour un salon donné.
    async fn play_turn(&self, reply: &Reply<'_>, mut state: ConversationState, action: &str) {
        let channel_id = reply.channel_id();
        reply.typing().await;

        match common::generate_story(&self.config, &mut state, action).await {
            Ok(story) => {
                let image_enabled = state.image_enabled;
                if let Err(e) = self.store.save(channel_id, &state).await {
                    log::error!("Impossible de sauvegarder la session : {}", e);
                }
                reply.text(&story.story_text).await;
                self.maybe_send_image(reply, image_enabled, &story.scene_description).await;
            }
            Err(e) => {
                log::error!("Erreur lors de la génération de l'histoire : {}", e);
                reply
                    .text("🧛 La Bouche de l'Enfer brouille les ondes. Reformulez ou réécrivez votre dernière action !")
                    .await;
            }
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        log::info!("Connecté à Discord en tant que {}", ready.user.name);

        if let Err(e) = Command::set_global_commands(&ctx.http, slash_commands()).await {
            log::error!("Impossible d'enregistrer les slash commands : {}", e);
        } else {
            log::info!("Slash commands enregistrées.");
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        // Un message qui répond à un autre message du salon (un joueur qui
        // s'adresse à un autre joueur) n'est pas une action de jeu.
        if msg.referenced_message.is_some() {
            return;
        }
        // /ignore permet de parler aux autres joueurs du salon sans que
        // le message ne soit interprété comme une action du jeu.
        let text = msg.content.trim();
        if text.starts_with("/ignore") || text.starts_with("/i ") || text.starts_with('!') {
            return;
        }

        let lock = self.channel_lock(msg.channel_id).await;
        let _guard = lock.lock().await;

        // Pas d'aventure en cours dans ce salon : le bot reste silencieux.
        let state = match self.store.load(msg.channel_id).await {
            Some(state) => state,
            None => return,
        };

        if text.is_empty() {
            // Une image ou un fichier seul n'est pas une action ; en revanche un
            // contenu vide sur un message texte trahit un intent manquant.
            if msg.attachments.is_empty() && msg.embeds.is_empty() && msg.sticker_items.is_empty() {
                log::warn!(
                    "Message reçu sans contenu : l'intent privilégié MESSAGE_CONTENT est probablement désactivé \
                     dans le portail développeur Discord."
                );
            }
            return;
        }

        let reply = Reply::Channel(&ctx, msg.channel_id);
        self.play_turn(&reply, state, text).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };

        // La génération peut prendre bien plus que les 3 secondes autorisées
        // pour une réponse immédiate : on diffère systématiquement.
        if let Err(e) = command.defer(&ctx.http).await {
            log::error!("Impossible de différer la réponse à l'interaction : {}", e);
            return;
        }

        let reply = Reply::Command(&ctx, &command);
        let options = command.data.options();

        match command.data.name.as_str() {
            "start" => {
                let lock = self.channel_lock(command.channel_id).await;
                let _guard = lock.lock().await;

                // Conserver le modèle et les préférences d'image de la session précédente
                let previous = self.store.load(command.channel_id).await;
                let mut state = ConversationState {
                    provider: previous.as_ref().and_then(|s| s.provider),
                    image_enabled: previous.as_ref().map(|s| s.image_enabled).unwrap_or(true),
                    ..Default::default()
                };

                let initial_state = string_option(&options, "etat").unwrap_or("").trim();
                let start_msg = if initial_state.is_empty() {
                    "Commence une nouvelle aventure dans l'univers de Buffy contre les vampires.".to_string()
                } else {
                    format!(
                        "Commence une nouvelle aventure dans l'univers de Buffy contre les vampires. État initial : {}",
                        initial_state
                    )
                };

                reply
                    .text("🧛 La nuit tombe sur Sunnydale... Une nouvelle aventure commence.")
                    .await;
                reply.typing().await;

                match common::generate_story(&self.config, &mut state, &start_msg).await {
                    Ok(story) => {
                        let image_enabled = state.image_enabled;
                        if let Err(e) = self.store.save(command.channel_id, &state).await {
                            log::error!("Impossible de sauvegarder la session : {}", e);
                        }
                        reply.text(&story.story_text).await;
                        self.maybe_send_image(&reply, image_enabled, &story.scene_description).await;
                    }
                    Err(e) => {
                        log::error!("Erreur lors du démarrage du jeu : {}", e);
                        reply
                            .text("🧛 La Bouche de l'Enfer brouille les ondes (impossible de démarrer l'aventure). Réessayez !")
                            .await;
                    }
                }
            }
            "help" => {
                reply.text(HELP_TEXT).await;
            }
            "summary" => match self.store.load(command.channel_id).await {
                Some(state) => {
                    reply.typing().await;
                    match common::get_story_summary(&self.config, &state).await {
                        Ok(summary) => {
                            reply
                                .text(&format!(
                                    "📓 Journal de l'Observateur (copiez-le comme état initial de /start) :\n\n{}",
                                    summary
                                ))
                                .await;
                        }
                        Err(e) => {
                            log::error!("Erreur lors de la génération du résumé : {}", e);
                            reply.text("🧛 Impossible de rédiger le journal. Réessayez !").await;
                        }
                    }
                }
                None => {
                    reply
                        .text("Aucune aventure en cours dans ce salon. Tapez /start pour commencer !")
                        .await;
                }
            },
            "model" => {
                let provider = match string_option(&options, "modele").unwrap_or("") {
                    "gemini" => ModelProvider::Gemini,
                    "mistral" => ModelProvider::Mistral,
                    _ => {
                        reply.text("⚠️ Modèle inconnu. Choisissez gemini ou mistral.").await;
                        return;
                    }
                };

                if let Err(e) = self.config.key_for(provider) {
                    reply.text(&format!("⚠️ {}", e)).await;
                    return;
                }

                let lock = self.channel_lock(command.channel_id).await;
                let _guard = lock.lock().await;

                match self.store.load(command.channel_id).await {
                    Some(mut state) => {
                        state.provider = Some(provider);
                        if let Err(e) = self.store.save(command.channel_id, &state).await {
                            log::error!("Impossible de sauvegarder la session : {}", e);
                        }
                        reply
                            .text(&format!(
                                "🧛 Modèle changé : la suite de l'aventure sera racontée par {}.",
                                provider
                            ))
                            .await;
                    }
                    None => {
                        reply
                            .text("Aucune aventure en cours. Lancez /start, puis choisissez le modèle avec /model.")
                            .await;
                    }
                }
            }
            "image" => {
                let enabled = bool_option(&options, "actif").unwrap_or(true);

                let lock = self.channel_lock(command.channel_id).await;
                let _guard = lock.lock().await;

                match self.store.load(command.channel_id).await {
                    Some(mut state) => {
                        state.image_enabled = enabled;
                        if let Err(e) = self.store.save(command.channel_id, &state).await {
                            log::error!("Impossible de sauvegarder la session : {}", e);
                        }
                        let status = if enabled { "activée" } else { "désactivée" };
                        reply
                            .text(&format!("🧛 Génération d'images {} pour cette aventure.", status))
                            .await;
                    }
                    None => {
                        reply
                            .text("Aucune aventure en cours. Lancez /start, puis utilisez /image.")
                            .await;
                    }
                }
            }
            "deploy" => {
                let force = bool_option(&options, "force").unwrap_or(false);
                handle_deploy(&reply, command.user.id, force).await;
            }
            other => {
                log::warn!("Commande inconnue reçue : {}", other);
                reply.text("⚠️ Commande inconnue. Tapez /help pour la liste des commandes.").await;
            }
        }
    }
}

fn slash_commands() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("start")
            .description("Commencer une nouvelle aventure à Sunnydale.")
            .add_option(CreateCommandOption::new(
                CommandOptionType::String,
                "etat",
                "État initial facultatif (par exemple un résumé obtenu via /summary).",
            )),
        CreateCommand::new("help").description("Afficher l'aide."),
        CreateCommand::new("summary")
            .description("Obtenir un résumé complet de l'aventure pour la reprendre ailleurs."),
        CreateCommand::new("model")
            .description("Choisir le modèle IA qui raconte l'histoire.")
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "modele", "Modèle à utiliser.")
                    .required(true)
                    .add_string_choice("gemini", "gemini")
                    .add_string_choice("mistral", "mistral"),
            ),
        CreateCommand::new("image")
            .description("Activer ou désactiver la génération d'images.")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "actif",
                    "Vrai pour activer les images, faux pour les désactiver.",
                )
                .required(true),
            ),
        CreateCommand::new("deploy")
            .description("Déployer la dernière version du bot (admin seulement).")
            .add_option(CreateCommandOption::new(
                CommandOptionType::Boolean,
                "force",
                "Déployer malgré des modifications locales non commitées.",
            )),
    ]
}

fn string_option<'a>(options: &'a [ResolvedOption<'a>], name: &str) -> Option<&'a str> {
    options.iter().find(|o| o.name == name).and_then(|o| match &o.value {
        ResolvedValue::String(s) => Some(*s),
        _ => None,
    })
}

fn bool_option(options: &[ResolvedOption<'_>], name: &str) -> Option<bool> {
    options.iter().find(|o| o.name == name).and_then(|o| match &o.value {
        ResolvedValue::Boolean(b) => Some(*b),
        _ => None,
    })
}

const HELP_TEXT: &str = "🧛 Bienvenue sur la Bouche de l'Enfer ! 🧛\n\n\
     Vous vivez une aventure interactive dans l'univers de Buffy contre les vampires.\n\n\
     **Comment jouer :**\n\
     - Tapez `/start` pour lancer une nouvelle aventure. L'Observateur vous demandera votre nom, votre rôle et l'époque choisie.\n\
     - Décrivez ensuite vos actions librement dans le salon, ou choisissez parmi les options proposées.\n\
     - L'Observateur gère un système de dés (d20) pour les actions à enjeu : combats, rituels, filatures, négociations avec des démons...\n\
     - Préfixez un message par `/ignore` (ou `/i`, ou `!`) pour parler aux autres joueurs sans que le bot ne réagisse.\n\
     - Répondre à un message (reply) n'est jamais interprété comme une action de jeu.\n\n\
     **Commandes :**\n\
     `/start [etat]` — Commencer une nouvelle aventure avec un état initial facultatif\n\
     `/summary` — Obtenir le journal de l'Observateur, réutilisable comme état initial de `/start`\n\
     `/model gemini|mistral` — Choisir le modèle IA pour la suite de l'aventure\n\
     `/image actif:true|false` — Activer ou désactiver la génération d'images\n\
     `/deploy [force]` — Déployer la dernière version (admin)\n\
     `/help` — Afficher ce message d'aide";

// --- Messages d'erreur pour la génération d'images ---------------------------

static VISION_ERRORS: &[&str] = &[
    "⚠️ *La boule de cristal se voile.* La vision refuse de se former — trop d'interférences sur la Bouche de l'Enfer. L'histoire continue sans image.",
    "⚠️ *Le sortilège de projection a échoué.* Il manque un ingrédient au rituel, et personne n'a le temps de courir au magasin de magie. Pas d'image pour cette scène.",
    "⚠️ *Les archives du Conseil des Observateurs sont incomplètes.* Aucune illustration ne correspond à cette scène. Poursuivez.",
    "⚠️ *Une aura démoniaque brouille la vision.* Impossible de matérialiser l'image. La nuit continue.",
    "⚠️ *La transe s'interrompt brutalement.* La vision se dissipe avant d'avoir pris forme. L'histoire poursuit son cours.",
];

const VISION_BUDGET_ERROR: &str = "⚠️ *Le Conseil des Observateurs a gelé les fonds !* \
    Plus un centime pour les rituels de vision — il faudra renégocier le budget avant que les projections reprennent. \
    L'aventure continue sans images pour le moment.";

fn vision_error_message(err: &dyn std::fmt::Display) -> &'static str {
    let err_str = err.to_string();
    log::warn!("Erreur génération image : {}", err_str);
    if err_str.contains("402") {
        return VISION_BUDGET_ERROR;
    }
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % VISION_ERRORS.len();
    VISION_ERRORS[idx]
}

// --- Déploiement -------------------------------------------------------------

async fn handle_deploy(reply: &Reply<'_>, requester: UserId, force: bool) {
    // Vérification admin
    let admin_id = std::env::var("ADMIN_USER_ID")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    match admin_id {
        None => {
            reply
                .text("⚠️ ADMIN_USER_ID n'est pas configuré dans le .env — commande désactivée.")
                .await;
            return;
        }
        Some(admin) if admin == requester.get() => {}
        Some(_) => {
            reply.text("⛔ Accès refusé.").await;
            return;
        }
    }

    // Étape 1 : vérifier l'état du dépôt (staged + unstaged)
    if !force {
        let status = match tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                reply.text(&format!("❌ Impossible de lancer git : {}", e)).await;
                return;
            }
        };

        if !status.stdout.trim_ascii().is_empty() {
            let stat = String::from_utf8_lossy(&status.stdout);
            reply
                .text(&format!(
                    "⚠️ Des modifications locales existent (non commitées ou stagées) :\n\n{}\n\nUtilisez /deploy force:true pour déployer quand même.",
                    truncate_head(stat.trim(), 1500)
                ))
                .await;
            return;
        }
    }

    // Étape 2 : git pull
    reply.text("🔄 git pull en cours...").await;
    let pull = match tokio::process::Command::new("git").args(["pull"]).output().await {
        Ok(o) => o,
        Err(e) => {
            reply.text(&format!("❌ Impossible de lancer git pull : {}", e)).await;
            return;
        }
    };

    if !pull.status.success() {
        let err = String::from_utf8_lossy(&pull.stderr);
        reply
            .text(&format!("❌ git pull a échoué :\n\n{}", truncate_head(&err, 1800)))
            .await;
        return;
    }
    let pull_out = String::from_utf8_lossy(&pull.stdout);

    // Étape 3 : cargo build --release
    reply
        .text(&format!(
            "🔨 Compilation en cours (peut prendre plusieurs minutes)...\n{}",
            truncate_head(pull_out.trim(), 1500)
        ))
        .await;

    let cargo = cargo_path();
    let build = match tokio::process::Command::new(&cargo)
        .args(["build", "--release"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            reply
                .text(&format!(
                    "❌ Impossible de lancer cargo ({}) : {}\n\nDéfinissez CARGO_BIN dans le .env si cargo est installé ailleurs.",
                    cargo.to_string_lossy(),
                    e
                ))
                .await;
            return;
        }
    };

    if !build.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        reply
            .text(&format!(
                "❌ Compilation échouée — le service n'a PAS été redémarré, l'ancienne version continue de tourner.\n\n{}",
                truncate_tail(&stderr, 1700)
            ))
            .await;
        return;
    }

    // Étape 4 : redémarrage
    reply
        .text("✅ Compilation réussie ! Redémarrage du service dans 1 seconde...")
        .await;

    // On spawn pour laisser le temps à Discord de recevoir le message avant que le process meure.
    // Si le restart échoue (process toujours vivant), on notifie l'admin.
    let http = reply.ctx().http.clone();
    let channel_id = reply.channel_id();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let service = service_name();
        match tokio::process::Command::new("sudo")
            .args(["systemctl", "restart", &service])
            .output()
            .await
        {
            Err(e) => {
                log::error!("Impossible de lancer sudo systemctl restart : {}", e);
                let _ = channel_id
                    .say(&http, format!("❌ Redémarrage impossible : {}", e))
                    .await;
            }
            Ok(out) if !out.status.success() => {
                let err = String::from_utf8_lossy(&out.stderr);
                log::error!("systemctl restart a échoué : {}", err);
                let _ = channel_id
                    .say(&http, format!("❌ Redémarrage échoué :\n{}", truncate_tail(&err, 1000)))
                    .await;
            }
            Ok(_) => {} // succès → le process meurt, rien à envoyer
        }
    });
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

// --- Démarrage ---------------------------------------------------------------

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init();
    log::info!("Démarrage du bot Buffy contre les vampires...");

    let token = std::env::var("DISCORD_TOKEN")
        .or_else(|_| std::env::var("DISCORD_BOT_TOKEN"))
        .expect("DISCORD_TOKEN ou DISCORD_BOT_TOKEN doit être défini dans l'environnement ou le fichier .env");

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

    let handler = Handler {
        config,
        store: SessionStore::new(PathBuf::from("sessions")),
        locks: Mutex::new(HashMap::new()),
    };

    // MESSAGE_CONTENT est un intent privilégié : il doit être activé dans le
    // portail développeur Discord, sinon le bot ne verra pas les actions des joueurs.
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut discord = SerenityClient::builder(&token, intents)
        .event_handler(handler)
        .await
        .expect("Impossible de créer le client Discord");

    let shard_manager = discord.shard_manager.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            log::info!("Arrêt demandé, fermeture des connexions...");
            shard_manager.shutdown_all().await;
        }
    });

    if let Err(e) = discord.start().await {
        log::error!("Erreur du client Discord : {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_is_not_split() {
        let chunks = split_message("Bonjour Sunnydale", DISCORD_MSG_LIMIT);
        assert_eq!(chunks, vec!["Bonjour Sunnydale".to_string()]);
    }

    #[test]
    fn split_prefers_line_boundaries() {
        let text = format!("{}\n{}", "a".repeat(60), "b".repeat(60));
        let chunks = split_message(&text, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "a".repeat(60));
        assert_eq!(chunks[1], "b".repeat(60));
    }

    #[test]
    fn overlong_line_is_hard_split() {
        let chunks = split_message(&"x".repeat(250), 100);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 100));
        assert_eq!(chunks.concat().chars().count(), 250);
    }

    #[test]
    fn split_respects_char_boundaries() {
        // 150 caractères multi-octets : un découpage sur les octets paniquerait.
        let chunks = split_message(&"é".repeat(150), 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 100);
        assert_eq!(chunks[1].chars().count(), 50);
    }

    #[test]
    fn empty_text_produces_no_message() {
        assert!(split_message("   \n\n", DISCORD_MSG_LIMIT).is_empty());
    }
}
