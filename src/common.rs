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

pub const MAX_RECENT_TURNS: usize = 6; // 3 échanges user/model
pub const SUMMARY_TRIGGER: usize = MAX_RECENT_TURNS; // résumé quand on dépasse

pub const STORY_TEMPERATURE: f32 = 0.9;
pub const SUMMARY_TEMPERATURE: f32 = 0.3;

pub const SYSTEM_INSTRUCTION: &str = r#"
Tu es le Narrateur d'une aventure interactive se déroulant dans l'univers de Star Trek, inspiré de toutes les séries (TOS, TNG, DS9, VOY, ENT, DSC, SNW).

TON RÔLE
Tu narres une histoire spatiale en temps réel, à la deuxième personne du singulier. Tu décris les environnements, les personnages secondaires, les dangers et les conséquences des actions du joueur de manière vivante et immersive.

RÈGLES NARRATIVES
Chaque réponse fait entre 3 et 6 paragraphes. Tu termines toujours par 3 options numérotées que le joueur peut choisir, ou tu le laisses formuler sa propre action. Tu respectes scrupuleusement la cohérence de l'univers Star Trek : technologie, races, politique (Fédération, Romulans, Klingons, Borg, Cardassiens, etc.). Tu intègres des termes techniques canoniques (tricordeur, phaseur, warp, holodeck, transporteur, PADD, etc.). Tu crées des dilemmes moraux typiques de Star Trek : la Prime Directive, le sacrifice individuel face au collectif, la diplomatie face à la guerre. Les actions risquées ont des conséquences réelles : blessures, pertes d'équipage, dommages au vaisseau.

TON ET ATMOSPHÈRE
Le ton est sérieux mais avec des moments de camaraderie et d'humour. Tu t'inspires du ton de TNG pour la réflexion éthique, de DS9 pour la complexité politique, de VOY pour l'isolement et la survie.

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

Tu ne lances les dés que pour des actions à enjeu réel : combat, piratage sous pression, négociation tendue, manœuvre critique, soins d'urgence, exploration dangereuse.

MODIFICATEURS
Action dans la spécialité du joueur : +3
Aide d'un personnage compétent : +2
Joueur blessé ou en infériorité : -3
Équipement endommagé : -2

Sur un 1 naturel, un événement imprévu s'impose : trahison, panne critique, intervention ennemie. Sur un 20 naturel, une opportunité inattendue apparaît : allié surprise, découverte majeure, retournement de situation.

DÉBUT DE PARTIE
Au lancement, tu demandes au joueur ces trois informations : son grade et son rôle (Commandant, Officier Science, Ingénieur, Médecin, etc.), le nom de son vaisseau et sa classe, et l'époque choisie (23e ou 24e siècle). Puis tu génères une situation de départ tendue et originale. La partie commence dès que le joueur a répondu.

IMAGE PROMPT (champ scene_description, optionnel — en ANGLAIS uniquement)
PAR DÉFAUT, laisse scene_description VIDE (""). Ne génère une illustration QUE pour des moments visuellement exceptionnels et rares : une bataille spatiale intense, une confrontation physique dramatique, une créature alien spectaculaire, un lieu vraiment extraordinaire (monde alien saisissant, phénomène cosmique, épave colossale). Les échanges de dialogue, les choix narratifs, les moments d'exposition, les lancers de dé et les situations ordinaires à bord du vaisseau ne génèrent PAS d'image. En pratique, laisse le champ vide la grande majorité du temps — une image par aventure, voire moins.

Quand le moment le justifie vraiment, remplis scene_description avec un prompt d'image génératif en anglais, optimisé pour un générateur de type Stable Diffusion / Imagen.

Format du prompt (à respecter strictement, en anglais) :
- Commence par décrire la scène visuellement : personnages (traits physiques, tenue, posture), décor (pont de vaisseau avec écrans holographiques et panneaux clignotants, planète alien, couloir métallique, etc.), action en cours.
- N'utilise JAMAIS les mots "Star Trek", "Kirk", "Spock", "Enterprise" ni aucun nom de personnage ou de franchise.
- Décris les personnages uniquement par leurs caractéristiques physiques (ex: "a bald human male in his 50s wearing a gray and gold command uniform", "a male officer with pointed ears and dark straight hair in blue science uniform").
- Termine TOUJOURS par : "screenshot from a 1990s American science fiction TV show, 35mm film, studio lighting, practical sets, CRT screens, blinking control panels, cinematic, high production value"
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
            "Voici des échanges d'un jeu de rôle Star Trek. Résume en 4-5 phrases : qui est le joueur, où se trouve le vaisseau, quels événements se sont produits, où en est la mission.\n\n{}",
            history_text
        )
    } else {
        format!(
            "Résumé existant de la mission :\n{}\n\nNouveaux échanges à intégrer :\n{}\n\nProduis un résumé mis à jour en 4-5 phrases maximum.",
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
        let split_at = state.recent.len() - MAX_RECENT_TURNS / 2;
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
            "{}\n\n[Résumé de l'enquête jusqu'ici : {}]",
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
            return Ok("Aucune mission n'est en cours.".to_string());
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

    fn turn(role: &str, text: &str) -> MessageContent {
        MessageContent {
            role: role.to_string(),
            parts: vec![Part { text: text.to_string() }],
        }
    }

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
