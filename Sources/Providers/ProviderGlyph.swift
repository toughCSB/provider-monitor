import SwiftUI

/// Which mark a provider cell draws.
enum ProviderGlyph: String, Codable, Equatable {
    case claude
    case devin
    case openai
    case third
    case cursor
    /// The raw value stays `gemini`: it is the key archived readings were
    /// written under, and renaming it would make every stored reading for this
    /// provider undecodable.
    case antigravity = "gemini"
    /// Gemini's own sparkle, for the provider that meters a raw API key.
    ///
    /// It cannot be called `gemini`: that raw value already names Antigravity's
    /// arch inside every archived snapshot, and swapping its meaning would
    /// redraw old readings as a mark they were never written for. So the
    /// sparkle gets a key of its own instead.
    case geminiSpark = "gemini-spark"
    case glm
    case qwen
    case gemma
    case meta
    case deepseek
    case mistral
    case grok
    case opencode
    case commandcode
    case copilot
    case kimi
    case kiro
    case minimax
    case ollama
    case ollamaLocal = "ollama-local"
    case lmstudio

    /// If an asset with this name is in the bundle it wins over the traced
    /// outline — drop a PDF/SVG export from Figma in and it is picked up.
    var assetName: String { self == .ollamaLocal ? "glyph-ollama" : "glyph-\(rawValue)" }

    /// How much to scale this mark so it reads the same size as the others.
    ///
    /// Every outline is normalised into the same unit box, which makes their
    /// *boxes* identical and their marks anything but: measured on screen at
    /// 16pt, the OpenAI knot covered 32px while the Gemini spark covered 25 —
    /// a fifth smaller — because a spark's points are thin and its corners are
    /// mostly empty. Boxes of equal size are not marks of equal size, and the
    /// eye reads the mark.
    ///
    /// Measured from a render rather than guessed: each value brings that
    /// glyph's ink to the same extent as Claude's.
    var opticalScale: CGFloat {
        switch self {
        case .claude: return 0.97
        case .cursor: return 0.97
        case .openai: return 0.94
        case .antigravity: return 1.0
        case .geminiSpark: return 1.0
        case .glm:    return 0.95
        case .grok:   return 1.0
        // Now the vendor's own mark rather than a traced outline, so it is
        // measured with the rest of the bundled artwork.
        case .opencode: return 1.0
        case .commandcode: return 0.96
        case .copilot: return 0.96
        case .kimi:   return 0.95
        case .kiro:   return 0.95
        case .minimax: return 0.95
        case .ollama: return 0.95
        case .third:  return 1.0
        case .ollamaLocal: return 0.98
        case .lmstudio: return 0.96
        case .devin, .qwen, .gemma, .meta, .deepseek, .mistral: return 1.0
        }
    }

    var outline: [[CGPoint]] {
        switch self {
        case .claude: return GlyphOutline.claude
        case .openai: return GlyphOutline.openai
        case .third:  return GlyphOutline.third
        case .cursor: return GlyphOutline.cursor
        case .antigravity: return GlyphOutline.antigravity
        case .geminiSpark: return GlyphOutline.gemini
        case .glm:    return GlyphOutline.glm
        case .devin, .qwen, .gemma, .meta, .deepseek, .mistral, .lmstudio: return []
        case .grok:   return GlyphOutline.grok
        case .opencode: return GlyphOutline.opencode
        case .commandcode: return GlyphOutline.commandcode
        case .copilot: return GlyphOutline.copilot
        case .kimi:   return GlyphOutline.kimi
        case .kiro:   return GlyphOutline.kiro
        case .minimax: return GlyphOutline.minimax
        case .ollama, .ollamaLocal: return GlyphOutline.ollama
        }
    }
}

/// A traced outline scaled into the view's bounds, filled even-odd so the
/// counters inside a knot stay open.
struct GlyphShape: Shape {
    let outline: [[CGPoint]]

    func path(in rect: CGRect) -> Path {
        var path = Path()
        for loop in outline {
            guard let first = loop.first else { continue }
            path.move(to: point(first, in: rect))
            for p in loop.dropFirst() { path.addLine(to: point(p, in: rect)) }
            path.closeSubpath()
        }
        return path
    }

    private func point(_ p: CGPoint, in rect: CGRect) -> CGPoint {
        CGPoint(x: rect.minX + p.x * rect.width, y: rect.minY + p.y * rect.height)
    }
}

struct ProviderGlyphView: View {
    let glyph: ProviderGlyph
    var size: CGFloat = Design.px(46)
    var accountNumber: Int? = nil

    var body: some View {
        if let colour = brandColour {
            mark.foregroundStyle(colour)
        } else {
            mark
        }
    }

    /// The provider's own colour for its mark, or `nil` when the mark either has
    /// a colour of its own to keep (Antigravity's four-colour arch) or has none
    /// worth inventing (OpenCode, Ollama). `nil` leaves the caller's colour in
    /// charge, which is the app's text colour everywhere it is drawn today.
    private var brandColour: Color? {
        ProviderBrand.isFullColourMark(glyph) ? nil : ProviderBrand.markColor(glyph, accountNumber: accountNumber)
    }

    private var mark: some View {
        Group {
            if let image = NSImage(named: glyph.assetName) {
                Image(nsImage: image)
                    // Artwork in its own colours is drawn as it is; a one-colour
                    // mark is a stencil for whatever colour it is drawn in.
                    .renderingMode(ProviderBrand.isFullColourMark(glyph) ? .original : .template)
                    .resizable()
                    .scaledToFit()
            } else {
                GlyphShape(outline: glyph.outline)
                    .fill(style: FillStyle(eoFill: true))
            }
        }
        // Scaled inside a frame of the fixed size, so the *layout* stays on a
        // single grid — every row still reserves the same width — while the ink
        // is evened out within it.
        .scaleEffect(glyph.opticalScale)
        .frame(width: size, height: size)
    }
}
