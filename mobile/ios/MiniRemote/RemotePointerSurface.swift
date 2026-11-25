import SwiftUI
import UIKit

struct RemotePointerSurface: UIViewRepresentable {
    let tap: (CGPoint) -> Void
    let drag: (UIGestureRecognizer.State, CGPoint, CGSize) -> Void
    let pinch: (CGFloat, CGPoint) -> Void
    let twoFingerDrag: (CGSize) -> Void

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.isMultipleTouchEnabled = true
        let coordinator = context.coordinator

        let click = UITapGestureRecognizer(target: coordinator, action: #selector(Coordinator.tapped(_:)))
        let pointer = UIPanGestureRecognizer(target: coordinator, action: #selector(Coordinator.panned(_:)))
        pointer.maximumNumberOfTouches = 1
        let pan = UIPanGestureRecognizer(target: coordinator, action: #selector(Coordinator.pannedWithTwoFingers(_:)))
        pan.minimumNumberOfTouches = 2
        pan.maximumNumberOfTouches = 2
        let magnify = UIPinchGestureRecognizer(target: coordinator, action: #selector(Coordinator.magnified(_:)))

        click.require(toFail: pointer)
        for gesture in [click, pointer, pan, magnify] {
            gesture.delegate = coordinator
            view.addGestureRecognizer(gesture)
        }
        return view
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func updateUIView(_ view: UIView, context: Context) {
        context.coordinator.surface = self
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var surface: RemotePointerSurface
        private var isPinching = false
        private var isPanningWithTwoFingers = false

        init(_ surface: RemotePointerSurface) { self.surface = surface }

        @objc func tapped(_ recognizer: UITapGestureRecognizer) {
            guard !isPinching, !isPanningWithTwoFingers else { return }
            surface.tap(recognizer.location(in: recognizer.view))
        }

        @objc func panned(_ recognizer: UIPanGestureRecognizer) {
            let view = recognizer.view
            let delta = recognizer.translation(in: view)
            recognizer.setTranslation(.zero, in: view)
            let location = recognizer.state == .began
                ? CGPoint(x: recognizer.location(in: view).x - delta.x, y: recognizer.location(in: view).y - delta.y)
                : recognizer.location(in: view)
            let state = isPinching || isPanningWithTwoFingers ? .cancelled : recognizer.state
            surface.drag(state, location, CGSize(width: delta.x, height: delta.y))
        }

        @objc func pannedWithTwoFingers(_ recognizer: UIPanGestureRecognizer) {
            isPanningWithTwoFingers = recognizer.state == .began || recognizer.state == .changed
            let delta = recognizer.translation(in: recognizer.view)
            recognizer.setTranslation(.zero, in: recognizer.view)
            if isPanningWithTwoFingers && !isPinching {
                surface.twoFingerDrag(CGSize(width: delta.x, height: delta.y))
            }
        }

        @objc func magnified(_ recognizer: UIPinchGestureRecognizer) {
            isPinching = recognizer.state == .began || recognizer.state == .changed
            if isPinching {
                surface.pinch(recognizer.scale, recognizer.location(in: recognizer.view))
                recognizer.scale = 1
            }
        }

        func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer
        ) -> Bool {
            !(gestureRecognizer is UITapGestureRecognizer || otherGestureRecognizer is UITapGestureRecognizer)
        }
    }
}
