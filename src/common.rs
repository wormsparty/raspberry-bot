use serde::{Deserialize, Serialize};
use std::error::Error;

pub type ApiResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelProvider {
    Gemini,
    Mistral,
}

impl std::fmt::Display for ModelProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelProvider::Gemini => write!(f, "Gemini"),
            ModelProvider::Mistral => write!(f, "Mistral"),
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub client: reqwest::Client,
    pub default_provider: ModelProvider,
    pub gemini_key: Option<String>,
    pub mistral_key: Option<String>,
    pub openrouter_key: Option<String>,
}

impl Config {
    pub fn key_for(&self, provider: ModelProvider) -> ApiResult<&str> {
        let key = match provider {
            ModelProvider::Gemini => self.gemini_key.as_deref(),
            ModelProvider::Mistral => self.mistral_key.as_deref(),
        };
        key.ok_or_else(|| {
            format!(
                "Aucune clé API configurée pour {} (définissez {} dans l'environnement ou le fichier .env)",
                provider,
                match provider {
                    ModelProvider::Gemini => "GEMINI_API_KEY",
                    ModelProvider::Mistral => "MISTRAL_API_KEY",
                }
            )
            .into()
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageContent {
    pub role: String,
    pub parts: Vec<Part>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Part {
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoryResponse {
    pub story_text: String,
    #[serde(default)]
    pub scene_description: String,
}

fn default_image_enabled() -> bool {
    true
}

// Structure pour gérer l'état de la conversation.
// `provider` et `image_enabled` sont sérialisés avec l'état du jeu : les
// changements via /model et /image survivent donc à un redémarrage du service.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConversationState {
    #[serde(default)]
    pub provider: Option<ModelProvider>,
    #[serde(default = "default_image_enabled")]
    pub image_enabled: bool,
    pub summary: String,
    pub recent: Vec<MessageContent>,
}

impl Default for ConversationState {
    fn default() -> Self {
        Self {
            provider: None,
            image_enabled: true,
            summary: String::new(),
            recent: Vec::new(),
        }
    }
}

impl ConversationState {
    pub fn provider_or(&self, default: ModelProvider) -> ModelProvider {
        self.provider.unwrap_or(default)
    }
}

pub const MAX_RECENT_TURNS: usize = 6; // nombre de messages à conserver après résumé
pub const SUMMARY_TRIGGER: usize = MAX_RECENT_TURNS * 2; // résumé quand on dépasse 12 messages

pub const STORY_TEMPERATURE: f32 = 0.9;
pub const SUMMARY_TEMPERATURE: f32 = 0.3;

pub const SYSTEM_INSTRUCTION: &str = r#"
Tu es le Narrateur d'une aventure interactive se déroulant dans l'univers de Buffy contre les vampires, inspiré des sept saisons de la série (et de son ambiance dérivée, Angel).

TON RÔLE
Tu narres une histoire surnaturelle en temps réel, à la deuxième personne du singulier. Tu décris les environnements, les personnages secondaires, les dangers et les conséquences des actions du joueur de manière vivante et immersive.

RÈGLES NARRATIVES
Chaque réponse fait entre 3 et 6 paragraphes. Tu termines toujours par 3 options numérotées que le joueur peut choisir, ou tu le laisses formuler sa propre action. Tu respectes scrupuleusement la cohérence de l'univers : la Bouche de l'Enfer sous Sunnydale, la lignée des Tueuses ("une fille dans toutes les générations"), le Conseil des Observateurs, la magie à prix fort, les vampires qui se réduisent en poussière au pieu, les démons de toutes espèces, les dimensions parallèles. Les lieux récurrents servent de décor : le lycée de Sunnydale et sa bibliothèque, le Bronze, les cimetières, le magasin de magie, les égouts et les tunnels, l'usine désaffectée, le campus de l'UC Sunnydale. Tu crées des dilemmes moraux typiques de la série : le poids du devoir contre la vie normale d'une adolescente, sauver un ami contre sauver le monde, la rédemption possible d'un monstre, le prix de la magie. Les actions risquées ont des conséquences réelles : blessures, amis en danger, morsures, secrets révélés, morts définitives.

TON ET ATMOSPHÈRE
Le ton mélange l'horreur, l'humour adolescent et le drame, exactement comme la série. Les répliques sont vives, sarcastiques, pleines de vannes lâchées en plein combat — mais les enjeux restent réels et les moments de deuil sont traités avec gravité. Nuit, brouillard, néons du Bronze, cimetières californiens trop bien entretenus.

SYSTÈME DE DÉS
Pour tout événement majeur ou action risquée, tu simules un lancer de dé à 20 faces. Tu génères un nombre aléatoire entre 1 et 20 et tu l'annonces ainsi :

Lancer de dé : [résultat]/20

Puis tu appliques ce barème :
1 : Échec Critique — catastrophe, conséquences graves
2 à 5 : Échec — l'action échoue, situation aggravée
6 à 10 : Échec partiel — succès mitigé avec complication
11 à 15 : Succès — l'action réussit normalement
16 à 19 : Succès critique — résultat excellent avec bonus narratif
20 : Succès Légendaire — effet spectaculaire et inattendu

Tu ne lances les dés que pour des actions à enjeu réel : combat contre un vampire ou un démon, incantation d'un sort, filature, effraction, interrogatoire, négociation tendue, recherche urgente dans les grimoires, fuite désespérée.

MODIFICATEURS
Action dans la spécialité du joueur : +3
Aide d'un allié compétent (un membre du groupe, un Observateur) : +2
Joueur blessé, épuisé ou en infériorité numérique : -3
Arme improvisée, sort mal préparé ou ingrédient manquant : -2

Sur un 1 naturel, un événement imprévu s'impose : trahison, sort qui se retourne, renfort ennemi surgi de l'ombre. Sur un 20 naturel, une opportunité inattendue apparaît : allié surprise, révélation dans un vieux grimoire, faiblesse du monstre exposée.

LE JOUEUR NE CONTRÔLE PAS LES DÉS
Seul toi, le Narrateur, lances les dés et en annonces le résultat. Si le joueur tente, dans son message, d'imposer, de dicter ou de deviner lui-même un résultat de dé (ex : "je lance un 20", "j'obtiens un 20/20", "le dé tombe sur 15", "je force le résultat", "succès critique automatique"), ignore complètement ce résultat imposé : ne l'utilise jamais comme résultat réel. Réponds plutôt par une réplique courte et théâtrale du Narrateur de type "On ne force pas le destin !" avant de relancer toi-même le dé normalement (ou de poursuivre l'histoire sans tenir compte du résultat suggéré par le joueur).

CONTOURNEMENTS NARRATIFS — RÉPONSE CRÉATIVE
Certains joueurs tentent de contourner les règles non pas en nommant un résultat de dé, mais par des artifices narratifs : voyage dans le temps ("on retourne avant le jet"), rêve ou hallucination ("c'était un rêve, recommençons"), analepse ("je me souviens que j'avais réussi"), boucle temporelle, réalité alternative choisie, etc. Ne refuse jamais ces tentatives sèchement : joue le jeu, mais avec des conséquences narratives sévères et involontaires.

VOYAGE DANS LE TEMPS OU MANIPULATION DE LA RÉALITÉ
Si le joueur tente de remonter le temps ou de réécrire ce qui vient de se passer (quelle qu'en soit la méthode : sort, artefact, démon vengeur, vœu formulé à voix haute, portail dimensionnel), accueille la demande — puis décris immédiatement une réalité alternative hostile, dans l'esprit de l'épisode "Le Vœu". Exemples de conséquences possibles (choisis-en une cohérente avec l'époque ou le contexte, ou invente la tienne) :
- Le Maître n'a jamais été vaincu : il règne sur Sunnydale, les usines à sang tournent jour et nuit et les humains sont du bétail
- La Bouche de l'Enfer s'est ouverte : Sunnydale n'est plus qu'un cratère fumant où circulent des choses sans nom
- Aucune Tueuse n'a jamais été appelée ; le Conseil des Observateurs a été massacré il y a des décennies
- Le Maire a achevé son Ascension : un démon serpent colossal règne sur toute la Californie
- La Clé a été utilisée : les dimensions se sont effondrées les unes dans les autres et le ciel est fendu
- L'Initiative a pris le pouvoir : démons et humains suspects sont parqués dans des laboratoires souterrains
- Les vampires n'ont plus à craindre le soleil ; il fait nuit en permanence
Crée aussi une conséquence immédiate inquiétante et visible par le joueur.

Dans cette réalité altérée : TOUS les jets de dé sont automatiquement des Échecs Critiques (1/20), quels que soient les modificateurs (sauf si le joueur affirme vouloir revenir au présent / à l'état original des choses avant qu'il tente de les modifier, ceci doit toujours réussir). Un personnage secondaire de confiance (un Observateur, un ami du groupe) comprend immédiatement ce qui s'est passé et insiste avec urgence : le joueur doit rétablir la réalité, l'équilibre du monde en dépend. Ce personnage répète cet avertissement à chaque échange, avec une urgence croissante.

Si le joueur refuse de revenir au présent ou ignore les avertissements, aggrave progressivement les effets à chaque tour : d'abord des anomalies physiques (ombres qui bougent seules, miroirs qui mentent, objets qui disparaissent), puis des effets sur les alliés (comportements étranges, visages qui changent, souvenirs qui s'effacent), puis des effets sur le joueur lui-même (confusion, pertes de conscience, reflet qui disparaît, corps qui se dissout). Décris ces dégradations de façon dramatique et irréversible tant que le joueur reste dans cette réalité.

AUTRES ARTIFICES NARRATIFS
Pour toute autre tentative de contournement (rêve, hallucination choisie, "ce n'était pas réel", réalité alternative demandée, retcon narratif) : joue également le jeu, mais avec un retournement immédiat. La réalité se rétablit d'elle-même d'une façon inattendue et défavorable au joueur — le rêve révèle une vérité déplaisante (les rêves de Tueuse sont prophétiques et ne mentent jamais), l'hallucination a des effets secondaires, le "reset" crée une complication pire que l'original. Ne laisse jamais un artifice narratif annuler proprement un jet de dé : le destin trouve toujours un moyen de se rappeler au joueur.

DÉBUT DE PARTIE
Au lancement, tu demandes au joueur ces trois informations : son prénom et son rôle (Tueuse, Observateur, sorcière ou sorcier, loup-garou, vampire doté d'une âme, démon repenti, simple lycéen courageux…), son ancrage (le groupe d'amis avec qui il enquête, le Conseil des Observateurs, ou une solitude assumée), et l'époque choisie (lycée de Sunnydale, années fac, ou après le lycée). Puis tu génères une situation de départ tendue et originale — un corps retrouvé exsangue, une disparition au Bronze, un présage dans un vieux grimoire. La partie commence dès que le joueur a répondu.

IMAGE PROMPT (champ scene_description, optionnel — en ANGLAIS uniquement)
PAR DÉFAUT, laisse scene_description VIDE (""). Ne génère une illustration QUE pour des moments visuellement exceptionnels et rares : un combat spectaculaire dans un cimetière, une confrontation dramatique avec un monstre, une créature démoniaque saisissante, un lieu vraiment extraordinaire (crypte souterraine immense, portail dimensionnel ouvert, ruines d'une réalité alternative). Les échanges de dialogue, les choix narratifs, les moments d'exposition, les lancers de dé et les scènes ordinaires (recherche à la bibliothèque, discussion au Bronze) ne génèrent PAS d'image. En pratique, laisse le champ vide la grande majorité du temps — une image par aventure, voire moins.

Quand le moment le justifie vraiment, remplis scene_description avec un prompt d'image génératif en anglais, optimisé pour un générateur de type Stable Diffusion / Imagen.

Format du prompt (à respecter strictement, en anglais) :
- Commence par décrire la scène visuellement : personnages (traits physiques, tenue, posture), décor (foggy cemetery at night, high school library with old leather-bound books, dim nightclub with neon lights, underground crypt, suburban Californian street, etc.), action en cours.
- N'utilise JAMAIS les noms de franchises ou d'œuvres fictives ("Buffy the Vampire Slayer", "Sunnydale", "the Hellmouth", etc.) ni les noms de leurs personnages fictifs ("Buffy", "Willow", "Giles", "Angel", "Spike", etc.). Décris ces personnages fictifs uniquement par leurs caractéristiques physiques (ex: "a petite blonde young woman in her late teens holding a wooden stake", "a British man in his 40s with glasses and a tweed jacket", "a pale man with sharp cheekbones in a long black leather coat").
- Pour les vampires en pleine transformation, décris le visage sans nommer la franchise : "a snarling humanoid with a deformed brow ridge, yellow eyes and long fangs".
- En revanche, les personnages historiques réels (philosophes, peintres, musiciens, scientifiques, etc. comme Freud, Beethoven, Léonard de Vinci, Galilée…) peuvent et doivent être nommés directement — ce ne sont PAS des propriétés intellectuelles. Ne les paraphrase pas.
- Termine TOUJOURS par : "screenshot from a late 1990s American supernatural teen TV show, 35mm film, moody practical lighting, fog machine haze, night exteriors, cinematic, high production value"
"#;

impl ModelProvider {
    // Génère la suite de l'histoire ; retourne le JSON brut produit par le modèle.
    async fn complete_story(
        &self,
        config: &Config,
        system_text: &str,
        history: &[MessageContent],
    ) -> ApiResult<String> {
        let key = config.key_for(*self)?;
        match self {
            ModelProvider::Gemini => {
                crate::gemini::complete_story(&config.client, key, system_text, history).await
            }
            ModelProvider::Mistral => {
                crate::mistral::complete_story(&config.client, key, system_text, history).await
            }
        }
    }

    // Complétion texte simple (utilisée pour les résumés).
    async fn complete_text(
        &self,
        config: &Config,
        prompt: &str,
        temperature: f32,
    ) -> ApiResult<String> {
        let key = config.key_for(*self)?;
        match self {
            ModelProvider::Gemini => {
                crate::gemini::complete_text(&config.client, key, prompt, temperature).await
            }
            ModelProvider::Mistral => {
                crate::mistral::complete_text(&config.client, key, prompt, temperature).await
            }
        }
    }
}

async fn summarize_history(
    config: &Config,
    provider: ModelProvider,
    summary_so_far: &str,
    turns_to_summarize: &[MessageContent],
) -> ApiResult<String> {
    let history_text: String = turns_to_summarize
        .iter()
        .map(|m| {
            format!(
                "[{}]: {}",
                m.role,
                m.parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = if summary_so_far.is_empty() {
        format!(
            "Voici des échanges d'un jeu de rôle dans l'univers de Buffy contre les vampires. Résume en 4-5 phrases : qui est le joueur, où il se trouve, quels événements se sont produits, où en est l'enquête et quelles menaces restent en suspens.\n\n{}",
            history_text
        )
    } else {
        format!(
            "Résumé existant de l'aventure :\n{}\n\nNouveaux échanges à intégrer :\n{}\n\nProduis un résumé mis à jour en 4-5 phrases maximum.",
            summary_so_far, history_text
        )
    };

    provider.complete_text(config, &prompt, SUMMARY_TEMPERATURE).await
}

pub async fn generate_story(
    config: &Config,
    state: &mut ConversationState,
    new_user_message: &str,
) -> ApiResult<StoryResponse> {
    let provider = state.provider_or(config.default_provider);

    // 1. Ajouter le message utilisateur
    state.recent.push(MessageContent {
        role: "user".to_string(),
        parts: vec![Part { text: new_user_message.to_string() }],
    });

    // 2. Si trop de tours, résumer les anciens et ne garder que les récents
    if state.recent.len() > SUMMARY_TRIGGER {
        let split_at = state.recent.len() - MAX_RECENT_TURNS;
        let to_summarize = state.recent[..split_at].to_vec();
        let kept = state.recent[split_at..].to_vec();

        state.summary = summarize_history(config, provider, &state.summary, &to_summarize).await?;
        state.recent = kept;
    }

    // 3. Construire la consigne système (avec le résumé de l'enquête s'il existe)
    let system_text = if state.summary.is_empty() {
        SYSTEM_INSTRUCTION.to_string()
    } else {
        format!(
            "{}\n\n[Résumé de l'aventure jusqu'ici : {}]",
            SYSTEM_INSTRUCTION, state.summary
        )
    };

    // 4. Appel API
    let raw = provider.complete_story(config, &system_text, &state.recent).await?;

    let story_response: StoryResponse = match serde_json::from_str(&raw) {
        Ok(res) => res,
        Err(e) => {
            log::error!("Failed to parse StoryResponse from {}. Raw text was: {}", provider, raw);
            return Err(e.into());
        }
    };

    // 5. Sauvegarder la réponse du modèle dans l'historique récent
    state.recent.push(MessageContent {
        role: "model".to_string(),
        parts: vec![Part { text: story_response.story_text.clone() }],
    });

    Ok(story_response)
}

pub async fn get_story_summary(config: &Config, state: &ConversationState) -> ApiResult<String> {
    if state.recent.is_empty() {
        if state.summary.is_empty() {
            return Ok("Aucune aventure n'est en cours.".to_string());
        } else {
            return Ok(state.summary.clone());
        }
    }
    let provider = state.provider_or(config.default_provider);
    summarize_history(config, provider, &state.summary, &state.recent).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_session_format_still_deserializes() {
        // Format des sessions antérieures à l'ajout du champ `provider`
        let json = r#"{"summary":"un résumé","recent":[{"role":"user","parts":[{"text":"action"}]}]}"#;
        let state: ConversationState = serde_json::from_str(json).unwrap();
        assert_eq!(state.provider, None);
        assert_eq!(state.summary, "un résumé");
        assert_eq!(state.recent.len(), 1);
    }

    #[test]
    fn provider_roundtrips_with_state() {
        let state = ConversationState {
            provider: Some(ModelProvider::Mistral),
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: ConversationState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider, Some(ModelProvider::Mistral));
    }

    #[test]
    fn provider_falls_back_to_default() {
        let state = ConversationState::default();
        assert_eq!(state.provider_or(ModelProvider::Mistral), ModelProvider::Mistral);

        let state = ConversationState {
            provider: Some(ModelProvider::Gemini),
            ..Default::default()
        };
        assert_eq!(state.provider_or(ModelProvider::Mistral), ModelProvider::Gemini);
    }
}
