struct Interval: Codable {
    var speakerId: String
    var source: String
    var start: Double
    var end: Double
    var confidence: Double
    var isOverlap: Bool
    var candidateSpeakerIds: [String]
}

enum IntervalProcessing {
    static func prepare(_ input: [Interval]) -> [Interval] {
        // Normalize only before the split. A merge or duration filter after the
        // split can hide a short interruption or extend its overlap flag.
        splitOverlaps(normalize(input))
    }

    private static func normalize(_ input: [Interval]) -> [Interval] {
        let sorted = input
            .filter { $0.start.isFinite && $0.end.isFinite && $0.end - $0.start >= 0.25 }
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

    private struct Boundary {
        var starts: [Int] = []
        var ends: [Int] = []
    }

    private static func splitOverlaps(_ intervals: [Interval]) -> [Interval] {
        var boundaries: [Double: Boundary] = [:]
        for (index, interval) in intervals.enumerated() {
            boundaries[interval.start, default: Boundary()].starts.append(index)
            boundaries[interval.end, default: Boundary()].ends.append(index)
        }
        let times = boundaries.keys.sorted()
        var active = Set<Int>()
        var output: [Interval] = []
        for (start, end) in zip(times, times.dropFirst()) {
            guard let boundary = boundaries[start] else { continue }
            active.subtract(boundary.ends)
            active.formUnion(boundary.starts)
            let speakers = Set(active.map { intervals[$0].speakerId })
            for index in active.sorted() {
                var slice = intervals[index]
                slice.start = start
                slice.end = end
                slice.candidateSpeakerIds = speakers.filter { $0 != slice.speakerId }.sorted()
                slice.isOverlap = !slice.candidateSpeakerIds.isEmpty
                output.append(slice)
            }
        }
        return output
    }
}
