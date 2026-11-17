import SwiftUI

/// The colour each provider's own mark is drawn in.
///
/// Every mark used to be filled with the app's text colour, which is legible but
/// leaves shape as the only clue to which provider a cell belongs to — and at
/// the notch's 46 px, Cursor's, Codex's and Grok's marks are far more alike than
/// their names imply. A cell now wears its provider's colour instead.
///
/// Two different promises live in this table, and the difference is kept
/// deliberately visible:
///
/// * **The vendor's own colour** — Anthropic's Claude coral, DeepSeek's blue,
///   Google's blue for a Gemini API key, Meta's blue, Mistral's orange, Qwen's
///   violet.
/// * **A colour chosen for this app** — OpenAI, Cursor and xAI publish
///   monochrome-only logos, so those three are tinted with an accent picked
///   here. It is *not* an official brand colour and is never presented as one.
///   The app already refuses to pass a derived number off as an official one;
///   colours get the same honesty. These are the same three accents the Windows
///   port uses, so a provider looks the same on both.
///
/// `nil` is a decision as well: OpenCode, Ollama and LM Studio publish marks
/// that are already white, and tinting white only makes it a different white.
/// Those keep the app's text colour and look exactly as they did before.
enum ProviderBrand {

    /// The colour this glyph is drawn in, or `nil` to leave it in the app's own
    /// text colour — which is what every call site already sets.
    static func markColor(_ glyph: ProviderGlyph, accountNumber: Int? = nil) -> Color? {
        // Account accents, not alternative vendor branding. Quota risk colours stay unchanged.
        if accountNumber == 2 {
            if glyph == .openai { return Color(hex: 0x58C5AB) }
            if glyph == .claude { return Color(hex: 0xE9A080) }
        }
        switch glyph {
        // The vendor publishes this colour for the mark itself.
        case .claude:      return Color(hex: 0xD97757)   // Anthropic
        case .deepseek:    return Color(hex: 0x4D6BFE)
        case .geminiSpark: return Color(hex: 0x4285F4)   // Gemini API key
        case .meta:        return Color(hex: 0x0866FF)
        case .mistral:     return Color(hex: 0xFA520F)
        case .qwen:        return Color(hex: 0x615CED)

        // Monochrome-only logos: an accent chosen for this app, not a brand
        // colour. Kept identical to the Windows port's BRAND_TINT.
        case .openai:      return Color(hex: 0x10A37F)   // Codex
        case .cursor:      return Color(hex: 0x3B82F6)
        case .grok:        return Color(hex: 0x8B5CF6)

        // Nothing to add.
        case .antigravity,  // full-colour artwork — see `isFullColourMark`
             .opencode, .ollama, .ollamaLocal, .lmstudio,
             .devin, .third, .glm, .copilot, .kimi, .kiro, .minimax,
             .commandcode, .gemma:
            return nil
        }
    }

    /// Whether this mark ships as artwork in its own colours.
    ///
    /// Antigravity's official mark is a four-colour arch; a single tint cannot
    /// stand in for it, so it is bundled as an image and drawn untinted. Every
    /// other mark here is a stencil for one colour.
    static func isFullColourMark(_ glyph: ProviderGlyph) -> Bool {
        glyph == .antigravity
    }
}
