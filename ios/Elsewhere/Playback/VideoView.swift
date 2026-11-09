import AVFoundation
import SwiftUI
import UIKit

/// Shows the playlist player's video while it is on screen: the core draws each frame into
/// a pixel buffer, and an AVSampleBufferDisplayLayer shows it. When the view goes (the
/// player closes, the app goes to the background), video goes off and the sound plays on.
struct VideoView: UIViewRepresentable {
    /// The video's size as shown, to draw frames of its shape.
    let width: Int?
    let height: Int?

    func makeCoordinator() -> VideoFrames { VideoFrames() }

    func makeUIView(context: Context) -> LayerView {
        let view = LayerView()
        view.backgroundColor = .black
        view.display.videoGravity = .resizeAspect
        context.coordinator.layer = view.display
        context.coordinator.setSize(width: width, height: height)
        Playback.shared.attachVideo(context.coordinator)
        return view
    }

    func updateUIView(_ view: LayerView, context: Context) {
        context.coordinator.setSize(width: width, height: height)
    }

    static func dismantleUIView(_ view: LayerView, coordinator: VideoFrames) {
        Playback.shared.detachVideo(coordinator)
    }

    final class LayerView: UIView {
        override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
        var display: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
    }
}

/// Draws frames when the core says one is ready (on the core's thread: it only schedules),
/// one at a time on its own queue, and hands them to the layer.
final class VideoFrames: FrameListener {
    /// Frames are drawn at most this many pixels wide or tall (the layer scales them).
    private static let largest = 1920

    weak var layer: AVSampleBufferDisplayLayer?
    private let drawQueue = DispatchQueue(label: "elsewhere-video")
    private let lock = NSLock()
    private var pending = false
    private var size = (width: 1280, height: 720)
    private var pool: CVPixelBufferPool?
    private var poolSize = (width: 0, height: 0)
    private var format: CMVideoFormatDescription?

    /// The video's size (main thread), scaled down to `largest`.
    func setSize(width: Int?, height: Int?) {
        guard let width, let height, width > 0, height > 0 else { return }
        let scale = min(1, Double(Self.largest) / Double(max(width, height)))
        lock.lock()
        // Even sizes: some pixel formats want them.
        size = (max(2, Int(Double(width) * scale) & ~1), max(2, Int(Double(height) * scale) & ~1))
        lock.unlock()
    }

    func onFrame() {
        lock.lock()
        let busy = pending
        pending = true
        lock.unlock()
        // One drawing at a time; frames that come meanwhile are drawn by it (the newest).
        if !busy { drawQueue.async { self.draw() } }
    }

    private func draw() {
        lock.lock()
        pending = false
        let (width, height) = size
        lock.unlock()
        guard let layer, let buffer = pixelBuffer(width: width, height: height) else { return }
        CVPixelBufferLockBaseAddress(buffer, [])
        let base = CVPixelBufferGetBaseAddress(buffer)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        let drawn = base.map {
            Playback.shared.drawFrame(width: width, height: height, stride: stride,
                                      address: UInt64(UInt(bitPattern: $0)))
        } ?? false
        CVPixelBufferUnlockBaseAddress(buffer, [])
        guard drawn, let sample = sampleBuffer(buffer) else { return }
        if layer.status == .failed { layer.flush() }
        layer.enqueue(sample)
    }

    private func pixelBuffer(width: Int, height: Int) -> CVPixelBuffer? {
        if pool == nil || poolSize.width != width || poolSize.height != height {
            let attributes: [String: Any] = [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width,
                kCVPixelBufferHeightKey as String: height,
                kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
            ]
            pool = nil
            format = nil
            CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &pool)
            poolSize = (width, height)
        }
        guard let pool else { return nil }
        var buffer: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
        return buffer
    }

    private func sampleBuffer(_ buffer: CVPixelBuffer) -> CMSampleBuffer? {
        if format == nil {
            CMVideoFormatDescriptionCreateForImageBuffer(allocator: nil, imageBuffer: buffer, formatDescriptionOut: &format)
        }
        guard let format else { return nil }
        var timing = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: .zero, decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        CMSampleBufferCreateReadyWithImageBuffer(allocator: nil, imageBuffer: buffer, formatDescription: format,
                                                 sampleTiming: &timing, sampleBufferOut: &sample)
        guard let sample else { return nil }
        // Shown as soon as it comes: the core keeps the time.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let first = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(first, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}
