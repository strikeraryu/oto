import Foundation
import CoreAudio
import AudioToolbox
import Darwin

// This executable is embedded in the Rust CLI. stdout is a framed binary stream;
// stderr is reserved for errors. Core Audio callbacks never perform pipe IO.
struct EngineError: Error, CustomStringConvertible {
    let description: String
    init(_ message: String) { description = message }
}
func check(_ status: OSStatus, _ operation: String) throws {
    if status != noErr { throw EngineError("\(operation): Core Audio error \(status)") }
}
func address(_ selector: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain)
}
func property<T>(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector, _ initial: T, scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) throws -> T {
    var value = initial
    var addr = address(selector, scope)
    var size = UInt32(MemoryLayout<T>.size)
    try check(withUnsafeMutablePointer(to: &value) { AudioObjectGetPropertyData(id, &addr, 0, nil, &size, $0) }, "Read device property")
    return value
}
func stringProperty(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector) throws -> String {
    try property(id, selector, "" as CFString) as String
}
func ids(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) throws -> [AudioObjectID] {
    var addr = address(selector, scope)
    var size: UInt32 = 0
    try check(AudioObjectGetPropertyDataSize(id, &addr, 0, nil, &size), "Read device list size")
    if size == 0 { return [] }
    var result = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    try check(result.withUnsafeMutableBytes { AudioObjectGetPropertyData(id, &addr, 0, nil, &size, $0.baseAddress!) }, "Read device list")
    return result
}
struct Device: Codable {
    var id: UInt32
    var uid: String
    var name: String
    var sample_rate: Double
    var is_default: Bool
    var latency_frames: UInt32
}
func outputDevices() throws -> [Device] {
    let system = AudioObjectID(kAudioObjectSystemObject)
    let defaultID = try property(system, kAudioHardwarePropertyDefaultOutputDevice, AudioObjectID(0))
    return try ids(system, kAudioHardwarePropertyDevices).compactMap { id in
        guard !(try ids(id, kAudioDevicePropertyStreams, scope: kAudioDevicePropertyScopeOutput)).isEmpty else { return nil }
        return Device(id: id, uid: try stringProperty(id, kAudioDevicePropertyDeviceUID), name: try stringProperty(id, kAudioObjectPropertyName), sample_rate: try property(id, kAudioDevicePropertyNominalSampleRate, Double(0)), is_default: id == defaultID, latency_frames: (try? property(id, kAudioDevicePropertyLatency, UInt32(0), scope: kAudioDevicePropertyScopeOutput)) ?? 0)
    }
}
var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)
func nanos(_ ticks: UInt64) -> UInt64 {
    // Quotient/remainder avoids overflowing after long machine uptimes.
    let denom = UInt64(timebase.denom), numer = UInt64(timebase.numer)
    return (ticks / denom) * numer + (ticks % denom) * numer / denom
}
func frameIndex(_ ns: UInt64) -> Int64 {
    Int64(ns / 1_000_000_000) * 48_000 + Int64((ns % 1_000_000_000) * 48_000 / 1_000_000_000)
}
let recordLock = NSLock()
func writeRecord(_ kind: UInt32, _ timestamp: UInt64, _ payload: Data) throws {
    var header = Data()
    var k = kind.littleEndian, length = UInt32(payload.count).littleEndian, t = timestamp.littleEndian
    withUnsafeBytes(of: &k) { header.append(contentsOf: $0) }
    withUnsafeBytes(of: &length) { header.append(contentsOf: $0) }
    withUnsafeBytes(of: &t) { header.append(contentsOf: $0) }
    header.append(payload)
    recordLock.lock(); defer { recordLock.unlock() }
    try FileHandle.standardOutput.write(contentsOf: header)
}
func exactRead(_ count: Int) throws -> Data? {
    var data = Data()
    while data.count < count {
        guard let part = try FileHandle.standardInput.read(upToCount: count - data.count), !part.isEmpty else {
            if data.isEmpty { return nil }
            throw EngineError("Truncated engine input")
        }
        data.append(part)
    }
    return data
}
func u32(_ data: Data, _ at: Int) -> UInt32 { data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: at, as: UInt32.self).littleEndian } }
func u64(_ data: Data, _ at: Int) -> UInt64 { data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: at, as: UInt64.self).littleEndian } }

// Absolute sample tags make out-of-order writes safe. Unfilled slots render as
// silence. Resampling against the hardware host timestamp corrects output drift.
final class PlaybackRing {
    let lock = NSLock()
    let capacity = 96_000
    var left = [Float](repeating: 0, count: 96_000)
    var right = [Float](repeating: 0, count: 96_000)
    var tags = [Int64](repeating: -1, count: 96_000)
    func enqueue(_ timestamp: UInt64, _ data: Data) {
        let base = frameIndex(timestamp)
        lock.lock(); defer { lock.unlock() }
        data.withUnsafeBytes { bytes in
            for frame in 0..<(data.count / 4) {
                let tag = base + Int64(frame), slot = Int(tag % Int64(capacity))
                left[slot] = Float(Int16(littleEndian: bytes.loadUnaligned(fromByteOffset: frame * 4, as: Int16.self))) / 32768
                right[slot] = Float(Int16(littleEndian: bytes.loadUnaligned(fromByteOffset: frame * 4 + 2, as: Int16.self))) / 32768
                tags[slot] = tag
            }
        }
    }
    func render(_ buffers: UnsafeMutableAudioBufferListPointer, _ hostNS: UInt64, _ rate: Double) {
        for buffer in buffers { if let pointer = buffer.mData { memset(pointer, 0, Int(buffer.mDataByteSize)) } }
        guard lock.try() else { return }
        defer { lock.unlock() }
        guard let first = buffers.first, first.mNumberChannels > 0 else { return }
        let frames = Int(first.mDataByteSize) / (Int(first.mNumberChannels) * 4)
        let integer = frameIndex(hostNS)
        let fraction = Double((hostNS % 1_000_000_000) * 48_000 % 1_000_000_000) / 1_000_000_000
        for frame in 0..<frames {
            let position = fraction + Double(frame) * 48_000 / rate
            let offset = Int64(position), tag = integer + offset
            let a = Int(tag % Int64(capacity)), b = Int((tag + 1) % Int64(capacity))
            guard tags[a] == tag else { continue }
            let weight = Float(position - Double(offset))
            let l = left[a] + ((tags[b] == tag + 1 ? left[b] : left[a]) - left[a]) * weight
            let r = right[a] + ((tags[b] == tag + 1 ? right[b] : right[a]) - right[a]) * weight
            var channel = 0
            for buffer in buffers {
                if let samples = buffer.mData?.assumingMemoryBound(to: Float.self) {
                    for c in 0..<Int(buffer.mNumberChannels) {
                        samples[frame * Int(buffer.mNumberChannels) + c] = channel == 0 ? (buffers.count == 1 && buffer.mNumberChannels == 1 ? (l + r) / 2 : l) : (channel == 1 ? r : 0)
                        channel += 1
                    }
                }
            }
        }
    }
}

struct CapturedFrame { var left: Float; var right: Float; var time: UInt64 }
final class CaptureRing {
    let lock = NSLock()
    let capacity = 32_768
    var left = [Float](repeating: 0, count: 32_768)
    var right = [Float](repeating: 0, count: 32_768)
    var times = [UInt64](repeating: 0, count: 32_768)
    var read = 0, write = 0
    func capture(_ buffers: UnsafeMutableAudioBufferListPointer, _ time: UInt64, _ rate: Double) {
        guard lock.try() else { return }
        defer { lock.unlock() }
        guard let first = buffers.first, first.mNumberChannels > 0 else { return }
        let frames = Int(first.mDataByteSize) / (Int(first.mNumberChannels) * 4)
        for frame in 0..<frames {
            if write - read >= capacity { break }
            let slot = write % capacity
            var channels = 0, l: Float = 0, r: Float = 0
            for buffer in buffers {
                if let samples = buffer.mData?.assumingMemoryBound(to: Float.self) {
                    for c in 0..<Int(buffer.mNumberChannels) {
                        let sample = samples[frame * Int(buffer.mNumberChannels) + c]
                        if channels == 0 { l = sample }
                        if channels == 1 { r = sample }
                        channels += 1
                    }
                }
            }
            left[slot] = l; right[slot] = channels == 1 ? l : r
            times[slot] = time + UInt64(Double(frame) * 1_000_000_000 / rate)
            write += 1
        }
    }
    func drain() -> [CapturedFrame] {
        lock.lock(); defer { lock.unlock() }
        var frames: [CapturedFrame] = []
        frames.reserveCapacity(write - read)
        while read < write {
            let slot = read % capacity
            frames.append(CapturedFrame(left: left[slot], right: right[slot], time: times[slot]))
            read += 1
        }
        return frames
    }
}

@available(macOS 14.2, *)
final class Engine {
    let playback = PlaybackRing(), capture = CaptureRing()
    var output: AudioObjectID = 0, outputProc: AudioDeviceIOProcID?
    var tap: AudioObjectID = 0, aggregate: AudioObjectID = 0, captureProc: AudioDeviceIOProcID?
    var timer: DispatchSourceTimer?
    var device: Device?
    let controlQueue = DispatchQueue(label: "audio.oto.control")
    var defaultListener: AudioObjectPropertyListenerBlock?
    var followsDefault = true
    var stopping = false
    let writerQueue = DispatchQueue(label: "audio.oto.capture")
    var previous: CapturedFrame?
    var nextSampleNS: Double = 0
    var pending = Data(), packetTime: UInt64 = 0
    deinit { stop() }

    func startOutput(_ uid: String?) throws {
        let devices = try outputDevices()
        guard let selected = uid == nil ? devices.first(where: { $0.is_default }) : devices.first(where: { $0.uid == uid }) else { throw EngineError("Output device unavailable. Run oto devices and select a connected device.") }
        let format = try property(selected.id, kAudioDevicePropertyStreamFormat, AudioStreamBasicDescription(), scope: kAudioDevicePropertyScopeOutput)
        guard format.mFormatID == kAudioFormatLinearPCM, format.mFormatFlags & kAudioFormatFlagIsFloat != 0, format.mBitsPerChannel == 32, format.mSampleRate > 0 else { throw EngineError("Output must expose a Float32 PCM format") }
        followsDefault = uid == nil
        if selected.id == output && outputProc != nil { device = selected; return }
        stopOutput()
        output = selected.id
        let ring = playback, rate = format.mSampleRate
        try check(AudioDeviceCreateIOProcIDWithBlock(&outputProc, output, nil) { _, _, _, outputData, outputTime in
            ring.render(UnsafeMutableAudioBufferListPointer(outputData), nanos(outputTime.pointee.mHostTime), rate)
        }, "Create playback callback")
        do { try check(AudioDeviceStart(output, outputProc), "Start playback") }
        catch { stopOutput(); throw error }
        device = selected
    }
    func reportDevice() throws {
        if let device = device { try writeRecord(11, 0, JSONEncoder().encode(device)) }
    }
    func watchDefaultOutput() throws {
        var addr = address(kAudioHardwarePropertyDefaultOutputDevice)
        let listener: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.refreshDefaultOutput()
        }
        try check(AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &addr, controlQueue, listener), "Watch system output changes")
        defaultListener = listener
    }
    func refreshDefaultOutput(attempt: Int = 0) {
        guard followsDefault && !stopping else { return }
        do {
            let oldOutput = output
            try startOutput(nil)
            if oldOutput != output { try reportDevice() }
        } catch {
            // Bluetooth route transitions can briefly expose no usable default.
            if attempt < 8 {
                controlQueue.asyncAfter(deadline: .now() + .milliseconds(250)) { [weak self] in
                    self?.refreshDefaultOutput(attempt: attempt + 1)
                }
            } else {
                FileHandle.standardError.write(Data("Oto audio: could not follow system output: \(error)\n".utf8))
            }
        }
    }
    func startCapture() throws {
        // Output is already running, so this process has a Core Audio process ID.
        var pid = getpid(), processID: AudioObjectID = 0
        var addr = address(kAudioHardwarePropertyTranslatePIDToProcessObject)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        try check(AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, UInt32(MemoryLayout<pid_t>.size), &pid, &size, &processID), "Exclude Oto from system capture")
        guard processID != kAudioObjectUnknown else { throw EngineError("Oto audio process is not registered; cannot safely exclude playback from capture") }
        let description = CATapDescription(stereoGlobalTapButExcludeProcesses: [processID])
        description.name = "Oto system audio"
        description.isPrivate = true
        description.muteBehavior = .mutedWhenTapped
        try check(AudioHardwareCreateProcessTap(description, &tap), "Create system audio tap (grant System Audio Recording permission to Oto or your terminal)")
        let format = try property(tap, kAudioTapPropertyFormat, AudioStreamBasicDescription())
        guard format.mFormatID == kAudioFormatLinearPCM, format.mFormatFlags & kAudioFormatFlagIsFloat != 0, format.mBitsPerChannel == 32, format.mSampleRate > 0 else { throw EngineError("System tap must expose Float32 PCM") }
        let tapUID = try stringProperty(tap, kAudioTapPropertyUID)
        let config: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Oto private capture",
            kAudioAggregateDeviceUIDKey: "audio.oto.capture.\(UUID().uuidString)",
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceTapListKey: [[kAudioSubTapUIDKey: tapUID, kAudioSubTapDriftCompensationKey: true]],
        ]
        try check(AudioHardwareCreateAggregateDevice(config as CFDictionary, &aggregate), "Create private capture device")
        let ring = capture, rate = format.mSampleRate
        try check(AudioDeviceCreateIOProcIDWithBlock(&captureProc, aggregate, nil) { _, inputData, inputTime, _, _ in
            ring.capture(UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inputData)), nanos(inputTime.pointee.mHostTime), rate)
        }, "Create capture callback")
        let source = DispatchSource.makeTimerSource(queue: writerQueue)
        source.schedule(deadline: .now(), repeating: .milliseconds(2))
        source.setEventHandler { [weak self] in self?.flushCapture() }
        timer = source
        source.resume()
        try check(AudioDeviceStart(aggregate, captureProc), "Start system capture (check Privacy & Security → Screen & System Audio Recording)")
    }
    func flushCapture() {
        for current in capture.drain() {
            guard let last = previous else { previous = current; nextSampleNS = Double(current.time); continue }
            if current.time <= last.time || current.time - last.time > 2_000_000 {
                pending.removeAll(keepingCapacity: true)
                nextSampleNS = Double(current.time)
                previous = current
                continue
            }
            while nextSampleNS < Double(current.time) {
                let fraction = Float((nextSampleNS - Double(last.time)) / Double(current.time - last.time))
                if pending.isEmpty { packetTime = UInt64(nextSampleNS.rounded()) }
                for sample in [last.left + (current.left - last.left) * fraction, last.right + (current.right - last.right) * fraction] {
                    var pcm = Int16(max(-32768, min(32767, (sample.isFinite ? sample : 0) * 32767))).littleEndian
                    withUnsafeBytes(of: &pcm) { pending.append(contentsOf: $0) }
                }
                nextSampleNS += 1_000_000_000 / 48_000
                if pending.count == 960 {
                    do { try writeRecord(10, packetTime, pending) }
                    catch { exit(0) } // Parent disappeared; HAL tears down this process's private objects.
                    pending.removeAll(keepingCapacity: true)
                }
            }
            previous = current
        }
    }
    func stopOutput() {
        if let proc = outputProc { AudioDeviceStop(output, proc); AudioDeviceDestroyIOProcID(output, proc) }
        outputProc = nil
    }
    func stop() {
        stopping = true
        if let listener = defaultListener {
            var addr = address(kAudioHardwarePropertyDefaultOutputDevice)
            AudioObjectRemovePropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &addr, controlQueue, listener)
            defaultListener = nil
        }
        timer?.cancel(); timer = nil
        if let proc = captureProc { AudioDeviceStop(aggregate, proc); AudioDeviceDestroyIOProcID(aggregate, proc) }
        captureProc = nil
        if aggregate != 0 { AudioHardwareDestroyAggregateDevice(aggregate); aggregate = 0 }
        if tap != 0 { AudioHardwareDestroyProcessTap(tap); tap = 0 }
        stopOutput()
    }
    func run(capturing: Bool, uid: String?) throws {
        defer { controlQueue.sync { stop() } }
        try controlQueue.sync {
            try startOutput(uid)
            // Ready must precede capture packets on stdout.
            try reportDevice()
            try watchDefaultOutput()
            if capturing { try startCapture() }
            refreshDefaultOutput()
        }
        while let header = try exactRead(16) {
            let kind = u32(header, 0), count = Int(u32(header, 4)), time = u64(header, 8)
            guard count <= 4096, let data = try exactRead(count) else { throw EngineError("Invalid engine input frame") }
            switch kind {
            case 1:
                guard data.count == 960 else { throw EngineError("Invalid PCM frame") }
                playback.enqueue(time, data)
            case 2:
                let uid = String(data: data, encoding: .utf8)
                try controlQueue.sync {
                    try startOutput(uid?.isEmpty == true ? nil : uid)
                    try reportDevice()
                }
            case 3: return
            default: throw EngineError("Unknown engine command")
            }
        }
    }
}

do {
    let args = Array(CommandLine.arguments.dropFirst())
    if args.first == "devices" {
        try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(outputDevices()))
    } else if #available(macOS 14.2, *) {
        let engine = Engine()
        let uid = args.count > 1 && !args[1].isEmpty ? args[1] : nil
        try engine.run(capturing: args.first == "capture", uid: uid)
    } else { throw EngineError("Oto requires macOS 14.2 or later") }
} catch {
    FileHandle.standardError.write(Data("Oto audio: \(error)\n".utf8))
    exit(1)
}
