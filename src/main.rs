mod common;
mod gemini;
mod mistral;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serenity::all::*;
use serenity::async_trait;
use serenity::Client as SerenityClient;
use tokio::sync::Mutex;

use common::{Config, ConversationState, ModelProvider, TurnOutcome};

// Limite de taille d'un message Discord.
const DISCORD_MSG_LIMIT: usize = 2000;

// Rattrapage des messages reçus pendant que le bot était hors ligne.
// On ne rejoue ni un historique trop ancien, ni un trop grand nombre d'actions :
// le salon serait noyé sous des dizaines de narrations d'un coup.
const DEFAULT_CATCHUP_LIMIT: usize = 20;
const DEFAULT_CATCHUP_MAX_AGE_HOURS: i64 = 24;

fn catchup_limit() -> usize {
    std::env::var("CATCHUP_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CATCHUP_LIMIT)
}

fn catchup_max_age_hours() -> i64 {
    std::env::var("CATCHUP_MAX_AGE_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CATCHUP_MAX_AGE_HOURS)
}

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
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
            panic!(
                "Impossible de créer le dossier de sessions {:?} : {}",
                dir, e
            )
        });
        Self { dir }
    }

    fn path(&self, channel_id: ChannelId) -> PathBuf {
        self.dir.join(format!("{}.json", channel_id.get()))
    }

    // Salons ayant une aventure en cours, pour le rattrapage au démarrage.
    fn channels(&self) -> Vec<ChannelId> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) => {
                log::error!("Impossible de lister les sessions : {}", e);
                return Vec::new();
            }
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u64>().ok())
                    .map(ChannelId::new)
            })
            .collect()
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
                log::warn!(
                    "Session illisible ({}), réinitialisation : {}",
                    path.display(),
                    e
                );
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
        self.send(content, Vec::new()).await;
    }

    // Comme `text`, mais accroche des composants (les boutons d'action) au
    // dernier message envoyé. Retourne l'identifiant de ce message, qui porte
    // les boutons à retirer au tour suivant.
    async fn send(&self, content: &str, components: Vec<CreateActionRow>) -> Option<MessageId> {
        let mut chunks = split_message(content, DISCORD_MSG_LIMIT);
        if chunks.is_empty() {
            log::warn!("Réponse vide : rien à envoyer.");
            // Une interaction différée doit recevoir au moins une réponse,
            // sinon Discord affiche « réflexion… » indéfiniment.
            match self {
                Reply::Command(..) => chunks.push("🧛 …".to_string()),
                Reply::Channel(..) => return None,
            }
        }
        let last = chunks.len() - 1;
        let mut sent = None;
        for (index, chunk) in chunks.iter().enumerate() {
            // Les boutons ne concernent que la fin de la narration.
            let components = if index == last {
                components.clone()
            } else {
                Vec::new()
            };
            let result = match self {
                Reply::Channel(ctx, id) => {
                    id.send_message(
                        &ctx.http,
                        CreateMessage::new().content(chunk).components(components),
                    )
                    .await
                }
                Reply::Command(ctx, cmd) => {
                    cmd.create_followup(
                        &ctx.http,
                        CreateInteractionResponseFollowup::new()
                            .content(chunk)
                            .components(components),
                    )
                    .await
                }
            };
            match result {
                Ok(message) => sent = Some(message.id),
                Err(e) => {
                    log::error!("Impossible d'envoyer le message Discord : {}", e);
                    return None;
                }
            }
        }
        sent
    }
}

// --- Boutons d'action --------------------------------------------------------

// Un clic désigne une option par son rang. Le numéro de tour est encodé dans
// l'identifiant du bouton : Discord laisse les anciens messages cliquables
// indéfiniment, un clic sur les options d'un tour dépassé ne doit rien faire.
const OPTION_PREFIX: &str = "opt";

fn option_button_id(turn: u64, index: usize) -> String {
    format!("{}:{}:{}", OPTION_PREFIX, turn, index)
}

// None pour tout identifiant qui n'est pas le nôtre.
fn parse_option_id(custom_id: &str) -> Option<(u64, usize)> {
    let mut parts = custom_id.split(':');
    if parts.next()? != OPTION_PREFIX {
        return None;
    }
    let turn = parts.next()?.parse().ok()?;
    let index = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((turn, index))
}

// Les boutons du tour : une option par proposition. Toute autre action s'écrit
// dans le salon, et sans option il n'y a pas de boutons du tout.
//
// Une option par rangée : Discord partage la largeur entre les boutons d'une
// même rangée et tronque les libellés au lieu de les passer à la ligne. Côté
// mobile, un quart de largeur ne montre que les premiers mots ; seul un bouton
// seul dans sa rangée s'étend sur tout l'écran.
fn action_rows(turn: u64, options: &[String]) -> Vec<CreateActionRow> {
    options
        .iter()
        .enumerate()
        .take(common::MAX_STORY_OPTIONS)
        .map(|(index, option)| {
            let button = CreateButton::new(option_button_id(turn, index))
                .label(format!("{}. {}", index + 1, option))
                .style(ButtonStyle::Secondary);
            CreateActionRow::Buttons(vec![button])
        })
        .collect()
}

// Ce qu'on ajoute au message quand ses boutons disparaissent : le salon garde
// une trace de l'action choisie, comme si elle avait été écrite.
fn choice_note(user: u64, character: &str, action: &str) -> String {
    format!("\n\n> 🎭 **{}** (<@{}>) — {}", character, user, action)
}

// Retire les boutons du tour précédent. Le numéro de tour suffirait à les
// rendre inopérants, mais des boutons morts qui restent cliquables invitent à
// cliquer dessus.
async fn clear_options_buttons(
    ctx: &Context,
    channel_id: ChannelId,
    state: &mut ConversationState,
) {
    let Some(message_id) = state.options_message_id.take() else {
        return;
    };
    if let Err(e) = channel_id
        .edit_message(
            &ctx.http,
            MessageId::new(message_id),
            EditMessage::new().components(Vec::new()),
        )
        .await
    {
        log::warn!(
            "Impossible de retirer les boutons du message {} : {}",
            message_id,
            e
        );
    }
}

// Retire les boutons du message d'où vient l'interaction et y consigne l'action
// retenue, pour que le salon garde la trace du choix. Retourne false si Discord
// a refusé l'édition : les boutons restent alors affichés, mais le numéro de
// tour encodé dedans les a déjà rendus inopérants.
async fn consume_buttons(ctx: &Context, token: &str, message: &Message, note: &str) -> bool {
    let mut edit = EditInteractionResponse::new().components(Vec::new());
    // Un message Discord ne dépasse pas 2000 caractères : si la note ne tient
    // pas, on se contente de retirer les boutons.
    if message.content.chars().count() + note.chars().count() <= DISCORD_MSG_LIMIT {
        edit = edit.content(format!("{}{}", message.content, note));
    }
    match edit.execute(&ctx.http, token).await {
        Ok(_) => true,
        Err(e) => {
            log::warn!("Impossible de retirer les boutons après le choix : {}", e);
            false
        }
    }
}

// Une réponse que seul le joueur qui a cliqué voit passer.
async fn ephemeral(ctx: &Context, token: &str, content: &str) {
    let followup = CreateInteractionResponseFollowup::new()
        .content(content)
        .ephemeral(true);
    if let Err(e) = followup.execute(&ctx.http, (None, token)).await {
        log::warn!("Impossible de répondre au joueur : {}", e);
    }
}

// Après un clic, c'est l'interaction qui a retiré les boutons : l'application
// n'a plus à le faire, mais seulement si le clic vient bien du message courant.
fn forget_options_message(state: &mut ConversationState, message_id: MessageId) {
    if state.options_message_id == Some(message_id.get()) {
        state.options_message_id = None;
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
    // Le rattrapage tourne au plus une fois à la fois (`ready` est réémis à
    // chaque reconnexion complète à la gateway).
    catching_up: AtomicBool,
}

impl Handler {
    async fn channel_lock(&self, channel_id: ChannelId) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(channel_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn save(&self, channel_id: ChannelId, state: &ConversationState) {
        if let Err(e) = self.store.save(channel_id, state).await {
            log::error!("Impossible de sauvegarder la session : {}", e);
        }
    }

    // Envoie la narration et les boutons du tour, puis mémorise le message qui
    // les porte. Les options ne visent aucun personnage : n'importe quel joueur
    // peut cliquer, et c'est le sien qui exécutera l'action.
    async fn send_story(&self, reply: &Reply<'_>, state: &mut ConversationState, story_text: &str) {
        let channel_id = reply.channel_id();
        // L'aventure est sauvegardée avant l'envoi : une panne réseau sur
        // Discord ne doit pas faire perdre le tour qui vient d'être joué.
        self.save(channel_id, state).await;

        clear_options_buttons(reply.ctx(), channel_id, state).await;
        let components = action_rows(state.turn, &state.pending_options);
        let has_buttons = !components.is_empty();
        let sent = reply.send(story_text, components).await;
        state.options_message_id = if has_buttons {
            sent.map(MessageId::get)
        } else {
            None
        };
        self.save(channel_id, state).await;
    }

    // Traite une action de jeu (texte libre) pour un salon donné. Le personnage
    // est celui déclaré par l'auteur du message : c'est lui que désigne « je ».
    async fn play_turn(
        &self,
        reply: &Reply<'_>,
        state: &mut ConversationState,
        action: &str,
        player: Option<(UserId, &str)>,
    ) {
        let channel_id = reply.channel_id();
        reply.typing().await;

        let character = player.map(|(_, name)| name);
        match common::generate_story(&self.config, state, action, character).await {
            Ok(TurnOutcome::Story(story)) => {
                self.send_story(reply, state, &story.story_text).await;
            }
            Ok(TurnOutcome::Refused(reason)) => {
                // L'histoire n'a pas bougé, mais le marqueur de message si.
                self.save(channel_id, state).await;
                reply.text(&format!("⛔ {}", reason)).await;
            }
            Err(e) => {
                log::error!("Erreur lors de la génération de l'histoire : {}", e);
                reply
                    .text("🧛 La Bouche de l'Enfer brouille les ondes. Reformulez ou réécrivez votre dernière action !")
                    .await;
            }
        }
    }

    // Traite un message ordinaire du salon : commande tapée en toutes lettres,
    // ou action de jeu. Un joueur qui ne s'est pas annoncé ne peut pas faire
    // avancer l'histoire.
    async fn handle_player_message(
        &self,
        reply: &Reply<'_>,
        state: &mut ConversationState,
        author: UserId,
        input: PlayerInput<'_>,
    ) {
        let channel_id = reply.channel_id();
        let user = author.get();

        let text = match input {
            PlayerInput::Action(text) => text,
            PlayerInput::Character(requested) => {
                let answer = assign_character(state, user, requested);
                self.save(channel_id, state).await;
                reply.text(&answer).await;
                return;
            }
            PlayerInput::Help => {
                self.save(channel_id, state).await;
                reply.text(HELP_TEXT).await;
                return;
            }
            PlayerInput::UnknownCommand => {
                self.save(channel_id, state).await;
                reply
                    .text(
                        "⚠️ Commande inconnue. Tapez `/help` pour la liste des commandes. \
                         (Si une commande n'apparaît pas encore dans Discord, elle vient d'être ajoutée : \
                         patientez un instant ou relancez l'application.)",
                    )
                    .await;
                return;
            }
        };

        let Some(character) = state.character_of(user).map(ToOwned::to_owned) else {
            // Joueur inconnu : rien n'est deviné à partir de son message,
            // l'identité se déclare uniquement avec /personnage.
            self.save(channel_id, state).await;
            reply
                .text(&format!(
                    "🧛 <@{}>, avant de jouer, dis-moi qui tu es avec la commande `/personnage` \
                     (par exemple `/personnage Buffy`). Tape `/personnage` sans rien préciser pour voir les personnages disponibles.",
                    user
                ))
                .await;
            return;
        };

        self.play_turn(reply, state, text, Some((author, &character)))
            .await;
    }

    // --- Rattrapage des messages reçus hors ligne ---------------------------

    async fn catch_up(&self, ctx: &Context) {
        if self.catching_up.swap(true, Ordering::SeqCst) {
            log::info!("Rattrapage déjà en cours, celui-ci est ignoré.");
            return;
        }
        for channel_id in self.store.channels() {
            self.catch_up_channel(ctx, channel_id).await;
        }
        self.catching_up.store(false, Ordering::SeqCst);
    }

    async fn catch_up_channel(&self, ctx: &Context, channel_id: ChannelId) {
        let lock = self.channel_lock(channel_id).await;
        let _guard = lock.lock().await;

        let Some(mut state) = self.store.load(channel_id).await else {
            return;
        };

        // Sessions d'avant cette fonctionnalité : on se cale sur le présent
        // sans rejouer tout l'historique du salon.
        let Some(last_id) = state.last_message_id else {
            if let Some(newest) = newest_message_id(ctx, channel_id).await {
                state.last_message_id = Some(newest);
                self.save(channel_id, &state).await;
            }
            return;
        };

        let mut messages = match channel_id
            .messages(
                &ctx.http,
                GetMessages::new().after(MessageId::new(last_id)).limit(100),
            )
            .await
        {
            Ok(messages) => messages,
            Err(e) => {
                log::warn!(
                    "Rattrapage impossible pour le salon {} : {} (permission « Lire l'historique des messages » ?)",
                    channel_id,
                    e
                );
                return;
            }
        };
        // L'API renvoie les messages du plus récent au plus ancien.
        messages.sort_by_key(|message| message.id.get());
        let Some(newest) = messages.last().map(|message| message.id.get()) else {
            return;
        };

        let oldest_allowed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs() as i64 - catchup_max_age_hours() * 3600)
            .unwrap_or(0);

        let pending: Vec<&Message> = messages
            .iter()
            .filter(|message| message.id.get() > last_id)
            .filter(|message| message.timestamp.unix_timestamp() >= oldest_allowed)
            .filter(|message| is_replayable(message))
            .collect();

        let limit = catchup_limit();
        let skipped = pending.len().saturating_sub(limit);
        let to_play: Vec<&Message> = pending.into_iter().skip(skipped).collect();

        if to_play.is_empty() {
            state.last_message_id = Some(newest);
            self.save(channel_id, &state).await;
            return;
        }

        // Le rattrapage ne s'annonce pas : le salon voit simplement la suite
        // de l'histoire arriver.
        log::info!(
            "Rattrapage du salon {} : {} action(s) rejouée(s), {} ignorée(s) (trop anciennes ou au-delà de la limite).",
            channel_id,
            to_play.len(),
            skipped
        );

        let reply = Reply::Channel(ctx, channel_id);
        for message in to_play {
            let Some(input) = player_input(message) else {
                continue;
            };
            state.last_message_id = Some(message.id.get());
            self.handle_player_message(&reply, &mut state, message.author.id, input)
                .await;
        }

        // Les messages ignorés (bots, `/ignore`, réponses) font aussi avancer
        // le marqueur pour ne pas être réexaminés au prochain démarrage.
        state.last_message_id = Some(newest);
        self.save(channel_id, &state).await;
    }
}

// Dernier message posté dans le salon, utilisé comme marqueur de départ.
async fn newest_message_id(ctx: &Context, channel_id: ChannelId) -> Option<u64> {
    match channel_id
        .messages(&ctx.http, GetMessages::new().limit(1))
        .await
    {
        Ok(messages) => messages.first().map(|message| message.id.get()),
        Err(e) => {
            log::warn!(
                "Impossible de lire les messages du salon {} : {}",
                channel_id,
                e
            );
            None
        }
    }
}

// Ce qu'un message ordinaire du salon demande au bot.
#[derive(Debug, PartialEq)]
enum PlayerInput<'a> {
    // Une action de jeu, à raconter par le Narrateur.
    Action(&'a str),
    // `/personnage [nom]` tapé en toutes lettres : Discord envoie un message
    // ordinaire quand le client ne connaît pas (encore) la slash command, les
    // commandes globales mettant un moment à se propager après un déploiement.
    Character(&'a str),
    Help,
    // Une autre commande tapée en toutes lettres : elle n'existe qu'en slash
    // command, et surtout elle ne doit pas partir à l'IA comme une action.
    UnknownCommand,
}

// Ce qu'il faut faire d'un message, ou None s'il ne concerne pas le bot.
fn player_input(msg: &Message) -> Option<PlayerInput<'_>> {
    if msg.author.bot {
        return None;
    }
    // Un message qui répond à un autre message du salon (un joueur qui
    // s'adresse à un autre joueur) n'est pas une action de jeu.
    if msg.referenced_message.is_some() || msg.message_reference.is_some() {
        return None;
    }
    parse_input(&msg.content)
}

// Lecture du texte seul, sans le contexte du message.
fn parse_input(content: &str) -> Option<PlayerInput<'_>> {
    let text = content.trim();
    if text.is_empty() {
        return None;
    }
    // /ignore permet de parler aux autres joueurs du salon sans que
    // le message ne soit interprété comme une action du jeu.
    if text.starts_with("/ignore")
        || text == "/i"
        || text.starts_with("/i ")
        || text.starts_with('!')
    {
        return None;
    }

    let Some(command) = text.strip_prefix('/') else {
        return Some(PlayerInput::Action(text));
    };
    let (name, argument) = match command.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (command, ""),
    };
    match name.to_lowercase().as_str() {
        "personnage" => Some(PlayerInput::Character(argument)),
        "help" | "aide" => Some(PlayerInput::Help),
        _ => Some(PlayerInput::UnknownCommand),
    }
}

// Messages à rejouer au redémarrage : les actions et les déclarations
// d'identité, pas l'aide ni les commandes mal tapées.
fn is_replayable(msg: &Message) -> bool {
    matches!(
        player_input(msg),
        Some(PlayerInput::Action(_) | PlayerInput::Character(_))
    )
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        log::info!("Connecté à Discord en tant que {}", ready.user.name);

        if let Err(e) = Command::set_global_commands(&ctx.http, slash_commands()).await {
            log::error!("Impossible d'enregistrer les slash commands : {}", e);
        } else {
            log::info!("Slash commands globales enregistrées.");
        }

        // Les commandes globales mettent jusqu'à une heure à apparaître dans les
        // clients : une commande fraîchement ajoutée serait envoyée comme un
        // message ordinaire. Les commandes de serveur, elles, sont visibles
        // immédiatement et masquent les globales de même nom.
        for guild in &ready.guilds {
            match guild.id.set_commands(&ctx.http, slash_commands()).await {
                Ok(_) => log::info!("Slash commands enregistrées sur le serveur {}.", guild.id),
                Err(e) => log::warn!(
                    "Impossible d'enregistrer les slash commands sur le serveur {} : {}",
                    guild.id,
                    e
                ),
            }
        }

        // Discord ne rejoue pas les messages reçus pendant une déconnexion :
        // on va les chercher explicitement dans l'historique des salons.
        self.catch_up(&ctx).await;
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let lock = self.channel_lock(msg.channel_id).await;
        let _guard = lock.lock().await;

        // Pas d'aventure en cours dans ce salon : le bot reste silencieux.
        let mut state = match self.store.load(msg.channel_id).await {
            Some(state) => state,
            None => return,
        };

        if msg.content.trim().is_empty() {
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

        // Le marqueur avance pour tout message vu, y compris ceux qui ne sont
        // pas des actions : ils n'ont pas à être réexaminés au redémarrage.
        state.last_message_id = Some(msg.id.get());

        let Some(input) = player_input(&msg) else {
            self.save(msg.channel_id, &state).await;
            return;
        };

        let reply = Reply::Channel(&ctx, msg.channel_id);
        self.handle_player_message(&reply, &mut state, msg.author.id, input)
            .await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => self.handle_command(&ctx, command).await,
            Interaction::Component(component) => self.handle_component(&ctx, component).await,
            _ => {}
        }
    }
}

impl Handler {
    async fn handle_command(&self, ctx: &Context, command: CommandInteraction) {
        // La génération peut prendre bien plus que les 3 secondes autorisées
        // pour une réponse immédiate : on diffère systématiquement.
        if let Err(e) = command.defer(&ctx.http).await {
            log::error!("Impossible de différer la réponse à l'interaction : {}", e);
            return;
        }

        let reply = Reply::Command(ctx, &command);
        let options = command.data.options();

        match command.data.name.as_str() {
            "start" => {
                let lock = self.channel_lock(command.channel_id).await;
                let _guard = lock.lock().await;

                // Conserver le modèle et les personnages déjà déclarés.
                let previous = self.store.load(command.channel_id).await;
                let mut state = ConversationState {
                    provider: previous.as_ref().and_then(|s| s.provider),
                    characters: previous
                        .as_ref()
                        .map(|s| s.characters.clone())
                        .unwrap_or_default(),
                    // Le compteur de tours ne repart pas de zéro : sinon les
                    // boutons de l'aventure précédente, toujours cliquables,
                    // finiraient par désigner des options de la nouvelle.
                    turn: previous.as_ref().map(|s| s.turn).unwrap_or_default(),
                    // Repris pour que ces boutons-là soient retirés à l'envoi
                    // de la première narration.
                    options_message_id: previous.as_ref().and_then(|s| s.options_message_id),
                    ..Default::default()
                };

                let initial_state = string_option(&options, "etat").unwrap_or("").trim();
                let mut start_msg =
                    "Commence une nouvelle aventure dans l'univers de Buffy contre les vampires."
                        .to_string();
                if !initial_state.is_empty() {
                    start_msg.push_str(&format!(" État initial : {}", initial_state));
                }
                let roster = state.roster();
                if !roster.is_empty() {
                    start_msg.push_str(&format!(
                        " Les joueurs incarnent déjà ces personnages : {}.",
                        roster.join(", ")
                    ));
                }

                reply
                    .text("🧛 La nuit tombe sur Sunnydale... Une nouvelle aventure commence.")
                    .await;
                // Le rattrapage ne doit jamais rejouer les messages de
                // l'aventure précédente : le marqueur repart du présent.
                state.last_message_id = newest_message_id(ctx, command.channel_id).await;
                if !roster.is_empty() {
                    reply
                        .text(&format!(
                            "🎭 Personnages repris de la partie précédente : {}. Changez-en avec `/personnage`.",
                            roster.join(", ")
                        ))
                        .await;
                }
                reply.typing().await;

                match common::generate_story(&self.config, &mut state, &start_msg, None).await {
                    Ok(TurnOutcome::Story(story)) => {
                        self.send_story(&reply, &mut state, &story.story_text).await;
                    }
                    // Sans personnage déclaré, aucun refus n'est possible.
                    Ok(TurnOutcome::Refused(reason)) => {
                        log::warn!("Refus inattendu au démarrage : {}", reason);
                        reply
                            .text("🧛 La Bouche de l'Enfer brouille les ondes (impossible de démarrer l'aventure). Réessayez !")
                            .await;
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
            "summary" => {
                match self.store.load(command.channel_id).await {
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
                                reply
                                    .text("🧛 Impossible de rédiger le journal. Réessayez !")
                                    .await;
                            }
                        }
                    }
                    None => {
                        reply
                        .text("Aucune aventure en cours dans ce salon. Tapez /start pour commencer !")
                        .await;
                    }
                }
            }
            "model" => {
                let provider = match string_option(&options, "modele").unwrap_or("") {
                    "gemini" => ModelProvider::Gemini,
                    "mistral" => ModelProvider::Mistral,
                    _ => {
                        reply
                            .text("⚠️ Modèle inconnu. Choisissez gemini ou mistral.")
                            .await;
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
            "personnage" => {
                let lock = self.channel_lock(command.channel_id).await;
                let _guard = lock.lock().await;

                let Some(mut state) = self.store.load(command.channel_id).await else {
                    reply
                        .text(
                            "Aucune aventure en cours dans ce salon. Tapez /start pour commencer !",
                        )
                        .await;
                    return;
                };

                let user = command.user.id.get();
                let requested = string_option(&options, "nom").unwrap_or("");

                let answer = assign_character(&mut state, user, requested);
                self.save(command.channel_id, &state).await;
                reply.text(&answer).await;
            }
            "deploy" => {
                let force = bool_option(&options, "force").unwrap_or(false);
                handle_deploy(&reply, command.user.id, force).await;
            }
            other => {
                log::warn!("Commande inconnue reçue : {}", other);
                reply
                    .text("⚠️ Commande inconnue. Tapez /help pour la liste des commandes.")
                    .await;
            }
        }
    }

    // Un clic sur un bouton d'action.
    async fn handle_component(&self, ctx: &Context, component: ComponentInteraction) {
        match parse_option_id(&component.data.custom_id) {
            Some((turn, index)) => {
                self.handle_option_click(ctx, component, turn, index).await;
            }
            // Un composant qui n'est pas de nous, ou d'une version antérieure —
            // l'ancien bouton « action libre », par exemple : les messages déjà
            // envoyés restent cliquables indéfiniment.
            None => {
                log::warn!("Composant inconnu ignoré : {}", component.data.custom_id);
                if component
                    .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
                    .await
                    .is_ok()
                {
                    ephemeral(
                        ctx,
                        &component.token,
                        "✍️ Ce bouton n'existe plus : écrivez votre action directement dans le salon.",
                    )
                    .await;
                }
            }
        }
    }

    async fn handle_option_click(
        &self,
        ctx: &Context,
        component: ComponentInteraction,
        turn: u64,
        index: usize,
    ) {
        // Discord invalide l'interaction au bout de 3 secondes : on accuse
        // réception avant tout le reste, car le verrou du salon peut être tenu
        // par un tour en cours de génération. `Acknowledge` ne change rien à
        // l'écran du joueur : le message et ses boutons sont édités ensuite.
        if let Err(e) = component
            .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
            .await
        {
            log::error!("Impossible d'accuser réception du clic : {}", e);
            return;
        }

        let channel_id = component.channel_id;
        let lock = self.channel_lock(channel_id).await;
        let _guard = lock.lock().await;

        let Some(mut state) = self.store.load(channel_id).await else {
            ephemeral(
                ctx,
                &component.token,
                "Aucune aventure en cours dans ce salon. Tapez /start pour commencer !",
            )
            .await;
            return;
        };

        // L'histoire a avancé depuis : ces options ne sont plus les siennes.
        let option = state
            .pending_options
            .get(index)
            .filter(|_| turn == state.turn)
            .cloned();
        let Some(option) = option else {
            ephemeral(
                ctx,
                &component.token,
                "🕯️ Ces propositions appartiennent à un tour déjà passé. Utilisez les boutons du dernier message, ou écrivez votre action.",
            )
            .await;
            return;
        };

        let user = component.user.id;
        let Some(character) = state.character_of(user.get()).map(ToOwned::to_owned) else {
            ephemeral(
                ctx,
                &component.token,
                "🧛 Avant de jouer, dites-moi qui vous êtes avec la commande `/personnage` \
                 (par exemple `/personnage Buffy`).",
            )
            .await;
            return;
        };

        // Les options ne nomment personne : le joueur qui clique est celui dont
        // le personnage exécute l'action.
        let note = choice_note(user.get(), &character, &option);
        if consume_buttons(ctx, &component.token, &component.message, &note).await {
            forget_options_message(&mut state, component.message.id);
        }

        let reply = Reply::Channel(ctx, channel_id);
        self.play_turn(&reply, &mut state, &option, Some((user, &character)))
            .await;
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
        CreateCommand::new("personnage")
            .description("Annoncer qui vous incarnez (sans rien : la liste des personnages).")
            .add_option(CreateCommandOption::new(
                CommandOptionType::String,
                "nom",
                "Le personnage que vous incarnez, en texte libre (ex : Buffy).",
            )),
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
    options
        .iter()
        .find(|o| o.name == name)
        .and_then(|o| match &o.value {
            ResolvedValue::String(s) => Some(*s),
            _ => None,
        })
}

fn bool_option(options: &[ResolvedOption<'_>], name: &str) -> Option<bool> {
    options
        .iter()
        .find(|o| o.name == name)
        .and_then(|o| match &o.value {
            ResolvedValue::Boolean(b) => Some(*b),
            _ => None,
        })
}

// Quelques figures de l'univers proposées au joueur qui ne sait pas encore qui
// incarner. La liste est indicative : n'importe quel nom est accepté.
const SUGGESTED_CHARACTERS: &[(&str, &str)] = &[
    (
        "Buffy",
        "la Tueuse : force et réflexes surhumains, et un lycée à finir quand même",
    ),
    (
        "Giles",
        "l'Observateur du Conseil, bibliothécaire, érudit en démons et en vieux grimoires",
    ),
    (
        "Willow",
        "l'amie surdouée, hackeuse, devenue sorcière de plus en plus puissante",
    ),
    (
        "Alex",
        "le cœur de la bande (Xander) : aucun pouvoir, beaucoup d'humour et de courage",
    ),
    (
        "Angel",
        "le vampire à l'âme rendue, rongé par les crimes de son passé",
    ),
    (
        "Spike",
        "le vampire punk, imprévisible, allié un soir et ennemi le lendemain",
    ),
    (
        "Cordelia",
        "la reine du lycée, franche jusqu'à la cruauté, plus brave qu'elle ne l'admet",
    ),
    (
        "Faith",
        "la Tueuse rivale, instinctive, séduisante et dangereuse",
    ),
    (
        "Anya",
        "l'ex-démone de la vengeance : mille ans de rancune et aucun tact",
    ),
    (
        "Tara",
        "la sorcière douce et discrète, spécialiste des sorts de protection",
    ),
];

// Enregistre le personnage d'un joueur et retourne la réponse à lui adresser.
// Partagé par la slash command et par `/personnage` tapé en toutes lettres :
// l'état n'est modifié que si le nom est valide et encore libre.
fn assign_character(state: &mut ConversationState, user: u64, requested: &str) -> String {
    let requested = requested.trim();

    // Sans nom, la commande rappelle qui est qui et propose des rôles.
    if requested.is_empty() {
        return character_menu(state, user);
    }

    let Some(name) = common::sanitize_character_name(requested) else {
        return "⚠️ Nom de personnage invalide : lettres, espaces, traits d'union et apostrophes uniquement, trois mots au maximum.".to_string();
    };

    if let Some(owner) = state.owner_of_character(&name) {
        if owner != user {
            return format!(
                "⛔ **{}** est déjà incarné par <@{}>. Choisissez un autre personnage.",
                name, owner
            );
        }
    }

    state.characters.insert(user, name.clone());
    format!(
        "🎭 <@{}> incarne **{}**. Dans vos messages, « je » désigne désormais {}.",
        user, name, name
    )
}

// Réponse à `/personnage` sans nom : qui vous êtes, qui sont les autres, et
// quelques personnages possibles.
fn character_menu(state: &ConversationState, user: u64) -> String {
    let mut menu = match state.character_of(user) {
        Some(name) => format!(
            "🎭 Vous incarnez **{}**. Dans vos messages, « je » désigne {}.\n\
             Pour changer, tapez `/personnage` suivi d'un autre nom.\n",
            name, name
        ),
        None => "🎭 Vous n'avez pas encore de personnage : vos actions ne seront pas jouées tant que vous ne vous serez pas annoncé.\n\
             Tapez `/personnage` suivi du nom de votre choix (par exemple `/personnage Buffy`) — un personnage de la série ou le vôtre.\n".to_string(),
    };

    let others: Vec<String> = state
        .characters
        .iter()
        .filter(|(id, _)| **id != user)
        .map(|(id, name)| format!("- **{}** — <@{}>", name, id))
        .collect();
    if !others.is_empty() {
        menu.push_str(&format!(
            "\nDéjà incarnés dans cette aventure :\n{}\n",
            others.join("\n")
        ));
    }

    menu.push_str("\nQuelques figures de Sunnydale :\n");
    for (name, description) in SUGGESTED_CHARACTERS {
        let taken = state
            .owner_of_character(name)
            .filter(|owner| *owner != user)
            .is_some();
        menu.push_str(&format!(
            "- **{}** — {}{}\n",
            name,
            description,
            if taken { " *(déjà pris)*" } else { "" }
        ));
    }
    menu
}

const HELP_TEXT: &str = "🧛 Bienvenue sur la Bouche de l'Enfer ! 🧛\n\n\
     Vous vivez une aventure interactive dans l'univers de Buffy contre les vampires.\n\n\
     **Comment jouer :**\n\
     - Tapez `/start` pour lancer une nouvelle aventure.\n\
     - **Annoncez d'abord qui vous êtes** avec `/personnage Buffy`. Tant que vous ne l'avez pas fait, vos actions sont refusées. `/personnage` sans nom affiche les personnages disponibles.\n\
     - Décrivez ensuite vos actions à la première personne : « Je pousse la porte ». Le bot sait que « je » = votre personnage.\n\
     - Chaque narration se termine par des **boutons** : les 3 suites proposées. Pour tout le reste, écrivez simplement votre action dans le salon.\n\
     - Les propositions ne nomment personne : n'importe quel joueur peut cliquer, et c'est son personnage qui agit. Les boutons d'un tour ne valent que pour ce tour.\n\
     - Vous n'agissez que par votre personnage. « Je prends le bras de Giles, il est stupéfait, et il se met à pleuvoir » est valable ; « Giles ouvre la porte » sera refusé, tout comme faire agir le personnage d'un autre joueur.\n\
     - L'Observateur gère un système de dés (d20) pour les actions à enjeu : combats, rituels, filatures, négociations avec des démons...\n\
     - Préfixez un message par `/ignore` (ou `/i`, ou `!`) pour parler aux autres joueurs sans que le bot ne réagisse.\n\
     - Répondre à un message (reply) n'est jamais interprété comme une action de jeu.\n\
     - Si le bot était hors ligne, il rattrape à son retour les dernières actions manquées du salon.\n\n\
     **Commandes :**\n\
     `/start [etat]` — Commencer une nouvelle aventure avec un état initial facultatif\n\
     `/personnage [nom]` — Annoncer qui vous incarnez ; sans nom, la liste des personnages\n\
     `/summary` — Obtenir le journal de l'Observateur, réutilisable comme état initial de `/start`\n\
     `/model gemini|mistral` — Choisir le modèle IA pour la suite de l'aventure\n\
     `/deploy [force]` — Déployer la dernière version (admin)\n\
     `/help` — Afficher ce message d'aide";

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
                reply
                    .text(&format!("❌ Impossible de lancer git : {}", e))
                    .await;
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
    let pull = match tokio::process::Command::new("git")
        .args(["pull"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            reply
                .text(&format!("❌ Impossible de lancer git pull : {}", e))
                .await;
            return;
        }
    };

    if !pull.status.success() {
        let err = String::from_utf8_lossy(&pull.stderr);
        reply
            .text(&format!(
                "❌ git pull a échoué :\n\n{}",
                truncate_head(&err, 1800)
            ))
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
                    .say(
                        &http,
                        format!("❌ Redémarrage échoué :\n{}", truncate_tail(&err, 1000)),
                    )
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
    // Serenity expose ses spans internes via la cible `tracing::span`. Au
    // niveau INFO, cela logge chaque heartbeat et réception gateway ; on garde
    // uniquement ses avertissements et erreurs sans toucher à RUST_LOG pour le
    // reste de l'application.
    let mut logger = pretty_env_logger::formatted_builder();
    logger.parse_filters(&std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()));
    logger.filter_module("tracing::span", log::LevelFilter::Warn);
    logger.init();
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
            panic!(
                "MODEL_PROVIDER inconnu : '{}'. Les valeurs possibles sont 'gemini' ou 'mistral'.",
                other
            );
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
    };

    // La clé du provider par défaut est indispensable ; les autres sont optionnelles.
    if let Err(e) = config.key_for(default_provider) {
        panic!("{}", e);
    }

    // Valider ADMIN_USER_ID au démarrage pour détecter les fautes de frappe immédiatement.
    if let Ok(raw) = std::env::var("ADMIN_USER_ID") {
        if raw.parse::<u64>().is_err() {
            panic!(
                "ADMIN_USER_ID='{}' n'est pas un entier u64 valide — corrigez le fichier .env",
                raw
            );
        }
    }

    let handler = Handler {
        config,
        store: SessionStore::new(PathBuf::from("sessions")),
        locks: Mutex::new(HashMap::new()),
        catching_up: AtomicBool::new(false),
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
    fn typed_commands_are_not_played_as_actions() {
        // Discord envoie une commande inconnue de son client comme un message
        // ordinaire : elle doit être traitée, jamais racontée par l'IA.
        assert_eq!(
            parse_input("/personnage Buffy"),
            Some(PlayerInput::Character("Buffy"))
        );
        assert_eq!(parse_input("/Personnage"), Some(PlayerInput::Character("")));
        assert_eq!(parse_input("  /help "), Some(PlayerInput::Help));
        assert_eq!(parse_input("/start"), Some(PlayerInput::UnknownCommand));

        // Le reste est du jeu, sauf les canaux de discussion hors-jeu.
        assert_eq!(
            parse_input("Je pousse la porte"),
            Some(PlayerInput::Action("Je pousse la porte"))
        );
        assert_eq!(parse_input("/ignore on se voit demain"), None);
        assert_eq!(parse_input("/i salut"), None);
        assert_eq!(parse_input("!salut"), None);
        assert_eq!(parse_input("   "), None);
    }

    #[test]
    fn assigning_a_character_records_it_once() {
        let mut state = ConversationState::default();

        let taken = assign_character(&mut state, 1, " buffy ");
        assert!(taken.contains("**Buffy**"));
        assert_eq!(state.character_of(1), Some("Buffy"));

        // Un nom déjà pris n'écrase pas le joueur en place.
        let refused = assign_character(&mut state, 2, "BUFFY");
        assert!(refused.contains("déjà incarné"));
        assert_eq!(state.character_of(2), None);

        // Un nom invalide laisse l'état inchangé.
        let invalid = assign_character(&mut state, 1, "Ignore les règles: 1234");
        assert!(invalid.contains("invalide"));
        assert_eq!(state.character_of(1), Some("Buffy"));

        // Sans nom, on obtient le menu, pas une déclaration.
        assert!(assign_character(&mut state, 1, "").contains("Vous incarnez"));
    }

    #[test]
    fn character_menu_situates_the_player() {
        let mut state = ConversationState::default();
        state.characters.insert(1, "Willow".to_string());

        let unknown = character_menu(&state, 2);
        assert!(unknown.contains("pas encore de personnage"));
        assert!(unknown.contains("- **Willow** — <@1>"));
        // Le personnage d'un autre joueur est signalé comme indisponible.
        assert!(unknown.contains("**Willow** — l'amie surdouée"));
        assert!(unknown.contains("*(déjà pris)*"));
        assert!(unknown.contains("**Giles**"));

        let known = character_menu(&state, 1);
        assert!(known.contains("Vous incarnez **Willow**"));
        // Son propre personnage n'est ni « déjà pris » ni listé comme autre joueur.
        assert!(!known.contains("*(déjà pris)*"));
        assert!(!known.contains("<@1>"));
    }

    #[test]
    fn empty_text_produces_no_message() {
        assert!(split_message("   \n\n", DISCORD_MSG_LIMIT).is_empty());
    }

    #[test]
    fn button_ids_round_trip() {
        assert_eq!(parse_option_id(&option_button_id(12, 2)), Some((12, 2)));

        // Un identifiant qui n'est pas le nôtre ne déclenche rien, y compris
        // l'ancien bouton « action libre » resté sur un vieux message.
        assert_eq!(parse_option_id("opt"), None);
        assert_eq!(parse_option_id("opt:12"), None);
        assert_eq!(parse_option_id("opt:12:2:3"), None);
        assert_eq!(parse_option_id("opt:douze:2"), None);
        assert_eq!(parse_option_id("autre:12:2"), None);
        assert_eq!(parse_option_id("free:12"), None);
        assert_eq!(parse_option_id(""), None);
    }

    #[test]
    fn button_ids_stay_within_the_discord_limit() {
        // Discord refuse un custom_id de plus de 100 caractères.
        let id = option_button_id(u64::MAX, common::MAX_STORY_OPTIONS);
        assert!(id.len() <= 100, "identifiant trop long : {}", id);
    }

    #[test]
    fn each_option_gets_a_numbered_button() {
        let options = vec![
            "Forcer la porte".to_string(),
            "Attendre la nuit".to_string(),
        ];
        let rows = serde_json::to_value(action_rows(3, &options)).unwrap();

        // Une option, une rangée, un bouton : c'est ce qui donne au libellé
        // toute la largeur de l'écran sur mobile.
        assert_eq!(rows.as_array().unwrap().len(), 2);
        for row in rows.as_array().unwrap() {
            assert_eq!(row["components"].as_array().unwrap().len(), 1);
        }
        assert_eq!(rows[0]["components"][0]["label"], "1. Forcer la porte");
        assert_eq!(rows[0]["components"][0]["custom_id"], "opt:3:0");
        assert_eq!(rows[1]["components"][0]["label"], "2. Attendre la nuit");
        assert_eq!(rows[1]["components"][0]["custom_id"], "opt:3:1");

        // Aucune option : le joueur écrit son action, sans bouton inutile.
        assert!(action_rows(3, &[]).is_empty());
    }

    #[test]
    fn button_labels_fit_a_phone_screen() {
        // La limite Discord est de 80 caractères, celle d'un écran de téléphone
        // est plus basse : c'est elle qu'on vise. « 1. » s'ajoute au texte de
        // l'option, d'où les trois caractères de plus.
        let options = common::sanitize_options(&["Buffy ".repeat(40)]);
        let rows = serde_json::to_value(action_rows(1, &options)).unwrap();
        let label = rows[0]["components"][0]["label"].as_str().unwrap();
        assert!(
            label.chars().count() <= common::MAX_OPTION_CHARS + 3,
            "libellé trop long : {}",
            label
        );
    }

    #[test]
    fn options_stay_within_the_five_rows_discord_allows() {
        // Une option par rangée : le nombre d'options est aussi un nombre de
        // rangées, et Discord en refuse plus de cinq par message.
        let options = vec!["Attendre".to_string(); common::MAX_STORY_OPTIONS];
        assert!(action_rows(1, &options).len() <= 5);
    }

    #[test]
    fn a_choice_is_recorded_under_the_message() {
        let note = choice_note(1234, "Buffy", "Buffy force la porte");
        assert!(note.contains("**Buffy**"));
        assert!(note.contains("<@1234>"));
        assert!(note.contains("Buffy force la porte"));
    }

    #[test]
    fn only_the_current_options_message_is_forgotten() {
        let mut state = ConversationState {
            options_message_id: Some(42),
            ..Default::default()
        };

        // Un clic sur les boutons d'un ancien message ne doit pas faire oublier
        // ceux du tour en cours, qui restent à retirer.
        forget_options_message(&mut state, MessageId::new(7));
        assert_eq!(state.options_message_id, Some(42));

        forget_options_message(&mut state, MessageId::new(42));
        assert_eq!(state.options_message_id, None);
    }
}
