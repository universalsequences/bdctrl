import AppKit
import Metal
import simd

/// Rasterizes label strings once (CoreText via NSAttributedString) into a
/// shared texture atlas so labels render as instanced Metal quads instead of
/// AppKit views — no layout churn during pan/zoom, and they stay glued to the
/// canvas at full frame rate.
@MainActor
final class LabelAtlas {
    struct Entry {
        let uvMin: SIMD2<Float>
        let uvMax: SIMD2<Float>
        let sizePx: SIMD2<Float>
    }

    let texture: MTLTexture
    private var entries: [String: Entry] = [:]
    private var cursorX = 0, cursorY = 0, rowHeight = 0
    private let atlasSize = 2048
    private let scale: CGFloat = 2   // rasterize @2x for retina

    init?(device: MTLDevice) {
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
                                                                  width: atlasSize, height: atlasSize, mipmapped: false)
        descriptor.usage = .shaderRead
        guard let texture = device.makeTexture(descriptor: descriptor) else { return nil }
        self.texture = texture
    }

    func entry(key: String, text: String, font: NSFont) -> Entry? {
        if let existing = entries[key] { return existing }
        let attributed = NSAttributedString(string: text, attributes: [.font: font, .foregroundColor: NSColor.white])
        let bounds = attributed.size()
        let width = min(Int(ceil(bounds.width * scale)) + 2, 1400)
        let height = Int(ceil(bounds.height * scale)) + 2
        guard width > 2 else { return nil }
        if cursorX + width > atlasSize { cursorX = 0; cursorY += rowHeight + 1; rowHeight = 0 }
        guard cursorY + height <= atlasSize else { return nil }   // atlas full: skip label
        guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                      bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        context.scaleBy(x: scale, y: scale)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
        attributed.draw(at: CGPoint(x: 1 / scale, y: 1 / scale))
        NSGraphicsContext.restoreGraphicsState()
        if let data = context.data {
            texture.replace(region: MTLRegionMake2D(cursorX, cursorY, width, height),
                            mipmapLevel: 0, withBytes: data, bytesPerRow: width * 4)
        }
        let entry = Entry(uvMin: SIMD2(Float(cursorX) / Float(atlasSize), Float(cursorY) / Float(atlasSize)),
                          uvMax: SIMD2(Float(cursorX + width) / Float(atlasSize), Float(cursorY + height) / Float(atlasSize)),
                          sizePx: SIMD2(Float(width), Float(height)))
        entries[key] = entry
        cursorX += width + 1
        rowHeight = max(rowHeight, height)
        return entry
    }
}
