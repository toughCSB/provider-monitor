import AppKit

/// Borderless, non-activating panel that floats over everything, including the
/// menu bar and full-screen apps. Non-activating matters: glancing at your
/// usage must never take focus off what you were actually doing.
final class NotchPanel: NSPanel {
    /// Supplies the right-click menu. Handled here rather than on the content
    /// view because `NSWindow.sendEvent` sees every event first — the hosting
    /// view's hit test resolves to a SwiftUI-owned subview, which has no menu
    /// of its own and may consume the click before it reaches us.
    var contextMenuProvider: (() -> NSMenu?)?
    /// A left click on the visible chrome. Handled here for the same reason the
    /// menu is: the hit test lands on a SwiftUI subview that may consume it.
    var onClick: ((CGPoint, Int) -> Void)?
    /// Whether a plain press at this point may slide the notch. The press is
    /// still only *allowed* to become a drag: whether it does is decided by
    /// `PressGesture` below. Nil means anywhere.
    ///
    /// Needed because the panel's live region is the notch *and* whatever the
    /// hover card is showing, and a press on the card belongs to the card.
    var canDragAt: ((CGPoint) -> Bool)?
    /// A drag of the notch, reported as the raw pointer delta since the last
    /// event — not a cumulative offset, so the caller decides what "along the
    /// edge" means for the current one.
    var onDragStart: (() -> Void)?
    var onDrag: ((CGFloat, CGFloat) -> Void)?
    /// The drag ended. Where to persist the offset the drags above moved to.
    var onDragEnd: (() -> Void)?

    /// Supplies the events that follow the press. Non-nil only in tests, which
    /// use it to script a gesture there is no real event stream to produce.
    var dragEventSourceForTesting: (() -> NSEvent?)?

    override func sendEvent(_ event: NSEvent) {
        guard event.type == .rightMouseDown,
              let menu = contextMenuProvider?(),
              let view = contentView,
              // Only over the visible chrome; elsewhere the panel is a hole.
              view.hitTest(event.locationInWindow) != nil
        else { return super.sendEvent(event) }

        NSMenu.popUpContextMenu(menu, with: event, for: view)
    }

    override func mouseDown(with event: NSEvent) {
        guard let view = contentView, view.hitTest(event.locationInWindow) != nil else {
            return super.mouseDown(with: event)
        }
        // ⌥ is the gesture this has always had, so it still drags from the first
        // pixel. A plain press is not a drag until the pointer has said so —
        // otherwise every click on a ring would be a one-pixel slide.
        let options = event.modifierFlags.contains(.option)
        let mayDrag = onDrag != nil && (options || (canDragAt?(event.locationInWindow) ?? true))
        guard mayDrag else {
            onClick?(event.locationInWindow, event.clickCount)
            return
        }
        trackDrag(from: event, immediate: options)
    }

    /// Blocks on this window's own event stream until the button lifts, the
    /// standard AppKit pattern for a custom drag started from `mouseDown`.
    ///
    /// A press that never moves far enough is the click it began as, so it is
    /// delivered on release rather than on press. Waiting costs nothing: the
    /// finger is still down either way, and the alternative — firing the click
    /// first and apologising for it if a drag follows — cannot be undone, since
    /// the click refetches a provider.
    private func trackDrag(from event: NSEvent, immediate: Bool) {
        var gesture = PressGesture(immediate: immediate)
        if gesture.isDragging { onDragStart?() }
        // Screen coordinates, not the event's own `deltaX`/`deltaY`. The panel
        // moves under the pointer as the drag proceeds, so a window-relative
        // delta cancels itself out against the movement it just caused — the
        // notch would creep rather than follow. Screen coordinates are not the
        // panel's and do not care that it moved.
        var previous = NSEvent.mouseLocation
        while let next = nextDragEvent() {
            switch next.type {
            case .leftMouseDragged:
                let now = NSEvent.mouseLocation
                let dx = now.x - previous.x
                // `NSEvent`'s own `deltaY` grows as the pointer moves *down*, and
                // AppKit's y grows up, so the screen-coordinate difference is
                // negated to keep the same convention the caller reads.
                let dy = previous.y - now.y
                previous = now
                if gesture.dragged(dx: dx, dy: dy) { onDragStart?() }
                if gesture.isDragging { onDrag?(dx, dy) }
            case .leftMouseUp:
                if gesture.isDragging { onDragEnd?() } else { onClick?(event.locationInWindow, event.clickCount) }
                return
            default:
                break
            }
        }
        // The stream ran dry without a release — nothing more is coming, so the
        // press is settled as the click it looked like.
        if gesture.isDragging { onDragEnd?() } else { onClick?(event.locationInWindow, event.clickCount) }
    }

    /// The window's own queue in the app; nothing at all under test, where
    /// `nextEvent(matching:)` would block a suite that has no events to send.
    private func nextDragEvent() -> NSEvent? {
        if let dragEventSourceForTesting { return dragEventSourceForTesting() }
        guard !Runtime.isUnderTest else { return nil }
        return nextEvent(matching: [.leftMouseDragged, .leftMouseUp])
    }

    init(contentRect: NSRect) {
        super.init(
            contentRect: contentRect,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        level = .statusBar
        collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        isMovable = false
        isMovableByWindowBackground = false
        hidesOnDeactivate = false
        becomesKeyOnlyIfNeeded = true
        isReleasedWhenClosed = false
    }

    /// Whether the notch floats above other applications' windows.
    ///
    /// `.statusBar` is above every ordinary window and below the menu bar; a
    /// panel that has to take its turn goes to `.normal`, where another app's
    /// window covers it as it would cover anything else. `.floating` is not the
    /// middle ground it looks like — it still outranks every ordinary window,
    /// which is the thing being turned off.
    func apply(alwaysOnTop: Bool) {
        let wanted: NSWindow.Level = alwaysOnTop ? .statusBar : .normal
        guard level != wanted else { return }
        level = wanted
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}
