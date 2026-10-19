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
