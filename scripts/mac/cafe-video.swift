// Stitch PNG frames into an H.264 .mp4 with AVFoundation (no ffmpeg needed).
//   swift scripts/mac/cafe-video.swift <frames-dir> <out.mp4>
// <frames-dir>/frames.txt lists "file seconds" per line.
import AVFoundation
import AppKit

let args = CommandLine.arguments
guard args.count == 3 else { print("usage: cafe-video.swift <frames-dir> <out.mp4>"); exit(2) }
let dir = URL(fileURLWithPath: args[1]), out = URL(fileURLWithPath: args[2])
try? FileManager.default.removeItem(at: out)
let list = try String(contentsOf: dir.appendingPathComponent("frames.txt"), encoding: .utf8)
    .split(separator: "\n").map { $0.split(separator: " ") }.map { (String($0[0]), Int($0[1]) ?? 1) }
let (w, h, fps) = (1280, 720, 10)
let writer = try AVAssetWriter(outputURL: out, fileType: .mp4)
let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
    AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: w, AVVideoHeightKey: h,
    AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 2_500_000],
])
let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32ARGB, kCVPixelBufferWidthKey as String: w, kCVPixelBufferHeightKey as String: h,
])
writer.add(input)
guard writer.startWriting() else { print("cannot write: \(String(describing: writer.error))"); exit(1) }
writer.startSession(atSourceTime: .zero)

func buffer(_ file: String) -> CVPixelBuffer? {
    guard let img = NSImage(contentsOf: dir.appendingPathComponent(file)),
          let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
    var pb: CVPixelBuffer?
    CVPixelBufferCreate(nil, w, h, kCVPixelFormatType_32ARGB, nil, &pb)
    guard let px = pb else { return nil }
    CVPixelBufferLockBaseAddress(px, [])
    let ctx = CGContext(data: CVPixelBufferGetBaseAddress(px), width: w, height: h, bitsPerComponent: 8,
                        bytesPerRow: CVPixelBufferGetBytesPerRow(px), space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue)
    ctx?.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
    CVPixelBufferUnlockBaseAddress(px, [])
    return px
}
var n: Int64 = 0
for (file, secs) in list {
    guard let px = buffer(file) else { print("cannot read \(file)"); exit(1) }
    for _ in 0..<(secs * fps) {
        while !input.isReadyForMoreMediaData { Thread.sleep(forTimeInterval: 0.005) }
        adaptor.append(px, withPresentationTime: CMTime(value: n, timescale: CMTimeScale(fps)))
        n += 1
    }
}
input.markAsFinished()
let done = DispatchSemaphore(value: 0)
writer.finishWriting { done.signal() }
done.wait()
print("\(out.path): \(n / Int64(fps)) s, \(list.count) frames, status \(writer.status == .completed ? "ok" : "failed")")
