import Foundation
import FluidAudio

private struct Request: Codable {
    let action: String
    let meetingId: String?
    let modelCachePath: String
    let outputPath: String?
    let sources: [AudioSource]?
    let tuning: Tuning?
}

private struct Tuning: Codable {
    let clusteringThreshold: Double?
    let warmStartFa: Double?
    let warmStartFb: Double?
    let minSpeakers: Int?
    let maxSpeakers: Int?
    let numSpeakers: Int?
}

private struct AudioSource: Codable {
    let kind: String
    let path: String
    let forcedSpeakerId: String?
}

private struct Response: Codable {
    let ok: Bool
    let meetingId: String?
    let intervals: [Interval]
    let error: String?
}

@main
private enum MeetilyDiarizationSidecar {
    static func main() async {
        do {
            guard #available(macOS 14.4, *) else {
                throw SidecarError.unsupportedMacOS
            }
            let request = try JSONDecoder().decode(
                Request.self,
                from: FileHandle.standardInput.readDataToEndOfFile()
            )
            let manager = OfflineDiarizerManager(config: try configuration(from: request.tuning))
            let cache = URL(fileURLWithPath: request.modelCachePath, isDirectory: true)
            try FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true)
            try await manager.prepareModels(directory: cache)

            if request.action == "prepare" {
                try emit(Response(ok: true, meetingId: request.meetingId, intervals: [], error: nil))
                return
            }
            guard request.action == "process" else {
                throw SidecarError.invalidAction(request.action)
            }

            var intervals: [Interval] = []
            for source in request.sources ?? [] {
                do {
                    let result = try await manager.process(URL(fileURLWithPath: source.path))
                    intervals.append(contentsOf: result.segments.map { segment in
                        let rawId = source.forcedSpeakerId ?? "\(source.kind)-\(segment.speakerId)"
                        return Interval(
                            speakerId: rawId,
                            source: source.kind,
                            start: Double(segment.startTimeSeconds),
                            end: Double(segment.endTimeSeconds),
                            confidence: Double(segment.qualityScore),
                            isOverlap: false,
                            candidateSpeakerIds: []
                        )
                    })
                } catch OfflineDiarizationError.noSpeechDetected {
                    continue
                }
            }

            intervals = IntervalProcessing.prepare(intervals)
            let response = Response(
                ok: true,
                meetingId: request.meetingId,
                intervals: intervals,
                error: nil
            )
            if let path = request.outputPath {
                let data = try encoded(response)
                try data.write(to: URL(fileURLWithPath: path), options: .atomic)
            }
            try emit(response)
        } catch {
            let response = Response(
                ok: false,
                meetingId: nil,
                intervals: [],
                error: error.localizedDescription
            )
            try? emit(response)
            Foundation.exit(1)
        }
    }

    private static func configuration(from tuning: Tuning?) throws -> OfflineDiarizerConfig {
        var config = OfflineDiarizerConfig()
        // FluidAudio 0.12.4 interprets this as cosine similarity, despite its
        // distance comment. Its 0.6 default merged distinct meeting voices.
        // Keep speaker count automatic; the benchmark compares labels, not counts.
        config.clustering.threshold = 0.90
        guard let tuning else {
            try config.validate()
            return config
        }
        if let value = tuning.clusteringThreshold { config.clustering.threshold = value }
        if let value = tuning.warmStartFa { config.clustering.warmStartFa = value }
        if let value = tuning.warmStartFb { config.clustering.warmStartFb = value }
        if let value = tuning.minSpeakers { config.clustering.minSpeakers = value }
        if let value = tuning.maxSpeakers { config.clustering.maxSpeakers = value }
        if let value = tuning.numSpeakers { config.clustering.numSpeakers = value }
        try config.validate()
        return config
    }

    private static func encoded(_ response: Response) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(response)
    }

    private static func emit(_ response: Response) throws {
        FileHandle.standardOutput.write(try encoded(response))
        FileHandle.standardOutput.write(Data([0x0A]))
    }
}

private enum SidecarError: LocalizedError {
    case unsupportedMacOS
    case invalidAction(String)

    var errorDescription: String? {
        switch self {
        case .unsupportedMacOS:
            return "Speaker refinement requires macOS 14.4 or newer"
        case .invalidAction(let action):
            return "Unsupported sidecar action: \(action)"
        }
    }
}
