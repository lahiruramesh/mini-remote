import AVFoundation
import CoreMedia
import SwiftUI
import UIKit

/// Serializes arbitrary network chunks into H.264 access units and queues
/// them for display. `append(_:)` and `reset()` are safe from any queue; they
/// synchronously serialize through an internal media queue so caller-side
/// buffering cannot grow without bound. Callers should still use one media
/// worker queue to avoid blocking network callbacks while a sample is built.
final class RemoteVideo: @unchecked Sendable {
    private let mediaQueue = DispatchQueue(label: "mini-remote.video", qos: .userInitiated)
    private let queueKey = DispatchSpecificKey<UInt8>()
    private var parser = AnnexBParser()
    private var currentAccessUnit: [Data] = []
    private var currentAccessUnitBytes = 0
    private var currentAccessUnitHasVCL = false
    private var prefixNALs: [Data] = []
    private var prefixBytes = 0
    private var sequenceParameterSet: Data?
    private var pictureParameterSet: Data?
    private var formatDescription: CMVideoFormatDescription?
    private var nextPresentationTime = CMTime.invalid
    private var waitingForIDR = true
    private weak var displayLayer: AVSampleBufferDisplayLayer?
    private let framesPerSecond: Int32

    init(framesPerSecond: Int32 = 30) {
        self.framesPerSecond = max(1, min(framesPerSecond, 120))
        mediaQueue.setSpecific(key: queueKey, value: 1)
    }

    /// Appends one arbitrary piece of an Annex B byte stream. Chunk boundaries
    /// may split start codes, NAL units, and access units.
    func append(_ data: Data) {
        guard !data.isEmpty else { return }
        serialized {
            let nals = parser.append(data)
            if parser.overflowed {
                discardAccessUnit()
                displayLayer?.flushAndRemoveImage()
                resetPresentationTime()
                return
            }
            for nal in nals {
                consume(nal)
            }
        }
    }

    /// Drops parser/decoder state and waits for fresh parameter sets and an IDR.
    func reset() {
        serialized {
            resetState(clearParameterSets: true)
            displayLayer?.flushAndRemoveImage()
        }
    }

    fileprivate func attach(to layer: AVSampleBufferDisplayLayer) {
        serialized {
            guard displayLayer !== layer else { return }
            displayLayer = layer
            layer.flushAndRemoveImage()
            waitingForIDR = true
            var timebase: CMTimebase?
            let status = CMTimebaseCreateWithSourceClock(
                allocator: kCFAllocatorDefault,
                sourceClock: CMClockGetHostTimeClock(),
                timebaseOut: &timebase
            )
            if status == noErr, let timebase {
                CMTimebaseSetTime(timebase, time: CMClockGetTime(CMClockGetHostTimeClock()))
                CMTimebaseSetRate(timebase, rate: 1)
                layer.controlTimebase = timebase
            }
            resetPresentationTime()
        }
    }

    fileprivate func detach(from layer: AVSampleBufferDisplayLayer) {
        serialized {
            guard displayLayer === layer else { return }
            layer.flushAndRemoveImage()
            displayLayer = nil
            waitingForIDR = true
        }
    }

    private func serialized<T>(_ body: () -> T) -> T {
        if DispatchQueue.getSpecific(key: queueKey) != nil {
            return body()
        }
        return mediaQueue.sync(execute: body)
    }

    private func consume(_ nal: Data) {
        guard let header = nal.first else { return }
        let type = header & 0x1F

        switch type {
        case 7:
            if sequenceParameterSet != nal {
                sequenceParameterSet = nal
                formatDescription = nil
            }
            return
        case 8:
            if pictureParameterSet != nal {
                pictureParameterSet = nal
                formatDescription = nil
            }
            return
        case 9: // Access unit delimiter.
            finishAccessUnit()
            startAccessUnit(with: nal)
            return
        case 10, 11: // End of sequence / end of stream.
            if currentAccessUnitHasVCL {
                appendNALs(prefixNALs, toCurrent: true)
                prefixNALs.removeAll(keepingCapacity: true)
                appendNAL(nal)
                finishAccessUnit()
            }
            return
        default:
            break
        }

        let isVCL = (1...5).contains(type)
        if isVCL, type == 1 || type == 2 || type == 5,
           currentAccessUnitHasVCL,
           firstMacroblockInSlice(nal) == 0 {
            let nextPrefix = prefixNALs
            clearPrefix()
            finishAccessUnit()
            appendNALs(nextPrefix, toCurrent: false)
        }

        if isVCL {
            if !currentAccessUnitHasVCL, !prefixNALs.isEmpty {
                appendNALs(prefixNALs, toCurrent: false)
                clearPrefix()
            }
            if appendNAL(nal) {
                currentAccessUnitHasVCL = true
            }
        } else if currentAccessUnitHasVCL {
            // SEI and similar prefix NALs may belong to the next picture when
            // encoders omit AUDs. Hold them until a first_mb_in_slice == 0.
            guard currentAccessUnitBytes + prefixBytes + nal.count + 4 <= Self.maximumAccessUnitBytes else {
                discardAccessUnit()
                return
            }
            prefixNALs.append(nal)
            prefixBytes += nal.count + 4
        } else {
            appendNAL(nal)
        }
    }

    private func startAccessUnit(with nal: Data) {
        currentAccessUnit.removeAll(keepingCapacity: true)
        currentAccessUnitBytes = 0
        currentAccessUnitHasVCL = false
        clearPrefix()
        appendNAL(nal)
    }

    @discardableResult
    private func appendNAL(_ nal: Data) -> Bool {
        guard currentAccessUnitBytes + nal.count + 4 <= Self.maximumAccessUnitBytes else {
            discardAccessUnit()
            return false
        }
        currentAccessUnit.append(nal)
        currentAccessUnitBytes += nal.count + 4
        return true
    }

    private func appendNALs(_ nals: [Data], toCurrent: Bool) {
        if !toCurrent, currentAccessUnitHasVCL { return }
        for nal in nals {
            if !appendNAL(nal) { break }
        }
    }

    private func finishAccessUnit() {
        guard currentAccessUnitHasVCL else {
            currentAccessUnit.removeAll(keepingCapacity: true)
            currentAccessUnitBytes = 0
            currentAccessUnitHasVCL = false
            return
        }

        let accessUnit = currentAccessUnit
        let keyFrame = accessUnit.contains { $0.first.map { ($0 & 0x1F) == 5 } ?? false }
        currentAccessUnit.removeAll(keepingCapacity: true)
        currentAccessUnitBytes = 0
        currentAccessUnitHasVCL = false
        enqueue(accessUnit, isKeyFrame: keyFrame)
    }

    private func enqueue(_ nals: [Data], isKeyFrame: Bool) {
        if let layer = displayLayer, layer.status == .failed {
            layer.flushAndRemoveImage()
            waitingForIDR = true
            resetPresentationTime()
        }

        guard let layer = displayLayer, let format = currentFormatDescription(),
              let blockBuffer = makeBlockBuffer(nals) else {
            waitingForIDR = true
            return
        }

        guard layer.isReadyForMoreMediaData else {
            // The display layer is backed up or has no decoder capacity. Flush
            // the stale queue and resume only on a random access frame.
            layer.flushAndRemoveImage()
            waitingForIDR = true
            resetPresentationTime()
            return
        }
        guard !waitingForIDR || isKeyFrame else { return }

        let duration = CMTime(value: 1, timescale: framesPerSecond)
        let clockTime = CMClockGetTime(CMClockGetHostTimeClock())
        if !nextPresentationTime.isValid || CMTimeCompare(nextPresentationTime, clockTime) < 0 {
            nextPresentationTime = clockTime
        }
        let presentationTime = nextPresentationTime
        var timing = CMSampleTimingInfo(
            duration: duration,
            presentationTimeStamp: presentationTime,
            decodeTimeStamp: .invalid
        )
        var sampleSize = CMBlockBufferGetDataLength(blockBuffer)
        var sampleBuffer: CMSampleBuffer?
        let status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: format,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )
        guard status == noErr, let sampleBuffer else {
            waitingForIDR = true
            return
        }
        markSyncState(sampleBuffer, isKeyFrame: isKeyFrame)
        layer.enqueue(sampleBuffer)
        if isKeyFrame { waitingForIDR = false }
        nextPresentationTime = CMTimeAdd(presentationTime, duration)
    }

    private func currentFormatDescription() -> CMVideoFormatDescription? {
        if let formatDescription { return formatDescription }
        guard let sps = sequenceParameterSet, let pps = pictureParameterSet else { return nil }

        var created: CMVideoFormatDescription?
        let status = sps.withUnsafeBytes { spsBytes -> OSStatus in
            pps.withUnsafeBytes { ppsBytes -> OSStatus in
                guard let spsBase = spsBytes.baseAddress,
                      let ppsBase = ppsBytes.baseAddress else { return -1 }
                var pointers: [UnsafePointer<UInt8>] = [
                    spsBase.assumingMemoryBound(to: UInt8.self),
                    ppsBase.assumingMemoryBound(to: UInt8.self),
                ]
                var sizes = [sps.count, pps.count]
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault,
                    parameterSetCount: 2,
                    parameterSetPointers: &pointers,
                    parameterSetSizes: &sizes,
                    nalUnitHeaderLength: 4,
                    formatDescriptionOut: &created
                )
            }
        }
        guard status == noErr, let created else { return nil }
        formatDescription = created
        return created
    }

    private func makeBlockBuffer(_ nals: [Data]) -> CMBlockBuffer? {
        let size = nals.reduce(0) { partial, nal in
            partial + MemoryLayout<UInt32>.size + nal.count
        }
        guard size > 0, size <= Self.maximumAccessUnitBytes else { return nil }
        var blockBuffer: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: size,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: size,
            flags: 0,
            blockBufferOut: &blockBuffer
        ) == kCMBlockBufferNoErr, let blockBuffer else { return nil }

        var bytes = Data(capacity: size)
        for nal in nals {
            var length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { bytes.append(contentsOf: $0) }
            bytes.append(nal)
        }
        let copied = bytes.withUnsafeBytes { rawBytes -> OSStatus in
            guard let base = rawBytes.baseAddress else { return -1 }
            return CMBlockBufferReplaceDataBytes(
                with: base,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: size
            )
        }
        return copied == kCMBlockBufferNoErr ? blockBuffer : nil
    }

    private func markSyncState(_ sampleBuffer: CMSampleBuffer, isKeyFrame: Bool) {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(
            sampleBuffer,
            createIfNecessary: true
        ), CFArrayGetCount(attachments) > 0,
        let value = CFArrayGetValueAtIndex(attachments, 0) else { return }
        let dictionary = unsafeBitCast(value, to: CFMutableDictionary.self)
        let notSync = isKeyFrame ? kCFBooleanFalse! : kCFBooleanTrue!
        CFDictionarySetValue(
            dictionary,
            Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque(),
            Unmanaged.passUnretained(notSync).toOpaque()
        )
        CFDictionarySetValue(
            dictionary,
            Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
            Unmanaged.passUnretained(kCFBooleanTrue!).toOpaque()
        )
    }

    private func discardAccessUnit() {
        currentAccessUnit.removeAll(keepingCapacity: true)
        currentAccessUnitBytes = 0
        currentAccessUnitHasVCL = false
        clearPrefix()
        waitingForIDR = true
    }

    private func clearPrefix() {
        prefixNALs.removeAll(keepingCapacity: true)
        prefixBytes = 0
    }

    private func resetState(clearParameterSets: Bool) {
        parser.reset()
        discardAccessUnit()
        waitingForIDR = true
        resetPresentationTime()
        if clearParameterSets {
            sequenceParameterSet = nil
            pictureParameterSet = nil
            formatDescription = nil
        }
    }

    private func resetPresentationTime() {
        if let timebase = displayLayer?.controlTimebase {
            nextPresentationTime = CMTimebaseGetTime(timebase)
        } else {
            nextPresentationTime = CMClockGetTime(CMClockGetHostTimeClock())
        }
    }

    private static let maximumAccessUnitBytes = 16 * 1024 * 1024
}

private struct AnnexBParser {
    private var bytes: [UInt8] = []
    private var scanOffset = 0
    private let maximumPendingBytes = 16 * 1024 * 1024
    private(set) var overflowed = false

    mutating func append(_ data: Data) -> [Data] {
        overflowed = false
        bytes.append(contentsOf: data)
        guard bytes.count <= maximumPendingBytes else {
            reset()
            overflowed = true
            return []
        }
        guard var current = startCode(in: bytes, from: 0) else {
            bytes = Array(bytes.suffix(3))
            scanOffset = 0
            return []
        }
        if current.index > 0 {
            bytes.removeFirst(current.index)
            scanOffset = 0
            current = startCode(in: bytes, from: 0) ?? current
        }

        var nals: [Data] = []
        var searchFrom = max(scanOffset, current.index + current.length)
        while let next = startCode(in: bytes, from: searchFrom) {
            let start = current.index + current.length
            if next.index > start {
                nals.append(Data(bytes[start..<next.index]))
            }
            current = next
            searchFrom = current.index + current.length
        }
        if current.index > 0 {
            bytes = Array(bytes[current.index...])
        }
        scanOffset = max(current.length, bytes.count - 3)
        if bytes.count > maximumPendingBytes {
            reset()
            overflowed = true
        }
        return nals
    }

    mutating func reset() {
        bytes.removeAll(keepingCapacity: true)
        scanOffset = 0
        overflowed = false
    }

    private func startCode(in bytes: [UInt8], from start: Int) -> (index: Int, length: Int)? {
        guard bytes.count >= 3 else { return nil }
        var index = max(0, start)
        while index + 2 < bytes.count {
            if bytes[index] == 0, bytes[index + 1] == 0 {
                if index + 3 < bytes.count, bytes[index + 2] == 0, bytes[index + 3] == 1 {
                    return (index, 4)
                }
                if bytes[index + 2] == 1 { return (index, 3) }
            }
            index += 1
        }
        return nil
    }
}

private func firstMacroblockInSlice(_ nal: Data) -> UInt32? {
    guard nal.count > 1 else { return nil }
    var rbsp: [UInt8] = []
    rbsp.reserveCapacity(min(16, nal.count - 1))
    var zeroCount = 0
    for byte in nal.dropFirst().prefix(16) {
        if zeroCount >= 2, byte == 3 {
            zeroCount = 0
            continue
        }
        rbsp.append(byte)
        zeroCount = byte == 0 ? zeroCount + 1 : 0
    }

    var bitIndex = 0
    var leadingZeros = 0
    while bitIndex < rbsp.count * 8 {
        let bit = (rbsp[bitIndex / 8] >> (7 - bitIndex % 8)) & 1
        bitIndex += 1
        if bit == 1 { break }
        leadingZeros += 1
        if leadingZeros > 31 { return nil }
    }
    guard leadingZeros <= 31, bitIndex <= rbsp.count * 8 else { return nil }
    var suffix: UInt32 = 0
    for _ in 0..<leadingZeros {
        guard bitIndex < rbsp.count * 8 else { return nil }
        suffix = (suffix << 1) | UInt32((rbsp[bitIndex / 8] >> (7 - bitIndex % 8)) & 1)
        bitIndex += 1
    }
    return ((UInt32(1) << leadingZeros) - 1) + suffix
}

final class SampleBufferView: UIView {
    weak var remoteVideo: RemoteVideo?
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    var displayLayer: AVSampleBufferDisplayLayer {
        layer as! AVSampleBufferDisplayLayer
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        backgroundColor = .black
        displayLayer.videoGravity = .resizeAspect
    }
}

struct DesktopVideoView: UIViewRepresentable {
    let video: RemoteVideo

    func makeUIView(context: Context) -> SampleBufferView {
        let view = SampleBufferView()
        view.backgroundColor = .black
        view.displayLayer.videoGravity = .resizeAspect
        view.remoteVideo = video
        video.attach(to: view.displayLayer)
        return view
    }

    func updateUIView(_ uiView: SampleBufferView, context: Context) {
        uiView.displayLayer.videoGravity = .resizeAspect
        uiView.remoteVideo = video
        video.attach(to: uiView.displayLayer)
    }

    static func dismantleUIView(_ uiView: SampleBufferView, coordinator: ()) {
        uiView.remoteVideo?.detach(from: uiView.displayLayer)
    }
}
