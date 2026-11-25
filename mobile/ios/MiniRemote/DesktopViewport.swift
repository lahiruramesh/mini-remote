import CoreGraphics

struct DesktopViewport {
    let container: CGSize
    let fitted: CGSize
    let zoom: CGFloat
    private(set) var offset: CGSize

    init(container: CGSize, resolution: CGSize, zoom: CGFloat, offset: CGSize) {
        self.container = container
        self.zoom = zoom
        let valid = resolution.width > 0 && resolution.height > 0
        let scale = valid ? min(container.width / resolution.width, container.height / resolution.height) : 1
        fitted = valid ? CGSize(width: resolution.width * scale, height: resolution.height * scale) : container
        self.offset = .zero
        self.offset = clamp(offset)
    }

    var displayed: CGSize {
        CGSize(width: fitted.width * zoom, height: fitted.height * zoom)
    }

    private var origin: CGPoint {
        CGPoint(x: (container.width - displayed.width) / 2 + offset.width,
                y: (container.height - displayed.height) / 2 + offset.height)
    }

    func clamp(_ proposed: CGSize) -> CGSize {
        let dx = max(0, (displayed.width - container.width) / 2)
        let dy = max(0, (displayed.height - container.height) / 2)
        return CGSize(width: min(max(proposed.width, -dx), dx), height: min(max(proposed.height, -dy), dy))
    }

    func location(of point: CGPoint) -> CGPoint {
        CGPoint(x: origin.x + point.x * displayed.width,
                y: origin.y + point.y * displayed.height)
    }

    func point(at location: CGPoint, clamped: Bool) -> CGPoint? {
        guard displayed.width > 0, displayed.height > 0 else { return nil }
        let x = (location.x - origin.x) / displayed.width
        let y = (location.y - origin.y) / displayed.height
        guard clamped || ((0...1).contains(x) && (0...1).contains(y)) else { return nil }
        return CGPoint(x: min(max(x, 0), 1), y: min(max(y, 0), 1))
    }

    func reveal(_ point: CGPoint) -> CGSize {
        let cursor = location(of: point)
        let horizontalInset = min(24, container.width / 2)
        let verticalInset = min(24, container.height / 2)
        let visibleX = min(max(cursor.x, horizontalInset), container.width - horizontalInset)
        let visibleY = min(max(cursor.y, verticalInset), container.height - verticalInset)
        return clamp(CGSize(width: offset.width + visibleX - cursor.x,
                            height: offset.height + visibleY - cursor.y))
    }

    static func isUltraWide(_ resolution: CGSize) -> Bool {
        resolution.height > 0 && resolution.width / resolution.height >= 2.1
    }

    static func focus(container: CGSize, resolution: CGSize, position: CGFloat) -> DesktopViewport {
        let overview = DesktopViewport(container: container, resolution: resolution, zoom: 1, offset: .zero)
        guard overview.fitted.height > 0 else { return overview }
        let zoom = min(3, max(1, container.height / overview.fitted.height))
        let horizontalTravel = max(0, overview.fitted.width * zoom - container.width)
        let offset = CGSize(width: (0.5 - position) * horizontalTravel, height: 0)
        return DesktopViewport(container: container, resolution: resolution, zoom: zoom, offset: offset)
    }
}
