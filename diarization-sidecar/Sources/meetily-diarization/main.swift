import Foundation
import FluidAudio

private struct Request: Codable {
    let action: String
    let meetingId: String?
    let modelCachePath: String
    let outputPath: String?
    let sources: [AudioSource]?
}

private struct AudioSource: Codable {
    let kind: String
    let path: String
    let forcedSpeakerId: String?
}

private struct Interval: Codable {
    var speakerId: String
    var source: String
    var start: Double
    var end: Double
    var confidence: Double
    var isOverlap: Bool
    var candidateSpeakerIds: [String]
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
            let manager = OfflineDiarizerManager()
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

            intervals = normalize(intervals)
            markOverlaps(&intervals)
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

    private static func normalize(_ input: [Interval]) -> [Interval] {
        let sorted = input
            .filter { $0.end - $0.start >= 0.25 }
            .sorted { ($0.start, $0.end) < ($1.start, $1.end) }
        var output: [Interval] = []
        for interval in sorted {
            if let last = output.last,
               last.speakerId == interval.speakerId,
               last.source == interval.source,
               interval.start - last.end <= 0.3
            {
                output[output.count - 1].end = max(last.end, interval.end)
                output[output.count - 1].confidence = max(last.confidence, interval.confidence)
            } else {
                output.append(interval)
            }
        }
        return output
    }

    private static func markOverlaps(_ intervals: inout [Interval]) {
        for first in intervals.indices {
            var candidates = Set<String>()
            for second in intervals.indices where first != second {
                guard intervals[first].speakerId != intervals[second].speakerId else { continue }
                let overlap = min(intervals[first].end, intervals[second].end)
                    - max(intervals[first].start, intervals[second].start)
                if overlap > 0.0 {
                    candidates.insert(intervals[second].speakerId)
                }
            }
            intervals[first].isOverlap = !candidates.isEmpty
            intervals[first].candidateSpeakerIds = candidates.sorted()
        }
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
