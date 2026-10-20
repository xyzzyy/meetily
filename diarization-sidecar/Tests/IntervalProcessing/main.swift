// Run without FluidAudio or downloaded models:
// swiftc Sources/meetily-diarization/IntervalProcessing.swift Tests/IntervalProcessing/main.swift -o /tmp/meetily-interval-tests
// /tmp/meetily-interval-tests

func interval(_ speaker: String, _ start: Double, _ end: Double, source: String = "system") -> Interval {
    Interval(
        speakerId: speaker, source: source, start: start, end: end,
        confidence: 0.8, isOverlap: false, candidateSpeakerIds: []
    )
}

func check(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
}

let interruption = IntervalProcessing.prepare([
    interval("a", 0, 60), interval("b", 10, 11),
])
let a = interruption.filter { $0.speakerId == "a" }
check(a.count == 3, "The long turn must split at both interruption boundaries")
check(a[0].start == 0 && a[0].end == 10 && !a[0].isOverlap, "Speech before interruption must not overlap")
check(a[1].start == 10 && a[1].end == 11 && a[1].isOverlap, "Only the interruption must overlap")
check(a[1].candidateSpeakerIds == ["b"], "Keep the other speaker as a candidate")
check(a[2].start == 11 && a[2].end == 60 && !a[2].isOverlap, "Speech after interruption must not overlap")
check(interruption.first { $0.speakerId == "b" }?.candidateSpeakerIds == ["a"], "Keep both same-source candidates")
check(a.reduce(0) { $0 + $1.end - $1.start } == 60, "Keep the full original turn")

let separate = IntervalProcessing.prepare([interval("a", 0, 10), interval("b", 10, 20)])
check(separate.count == 2 && separate.allSatisfy { !$0.isOverlap }, "Touching turns must not overlap")
check(IntervalProcessing.prepare([]).isEmpty, "Empty input must stay empty")

let crossSource = IntervalProcessing.prepare([
    interval("local-you", 0, 60, source: "microphone"), interval("remote", 10, 11),
])
let local = crossSource.filter { $0.source == "microphone" }
check(local.count == 3 && local.filter { $0.isOverlap }.count == 1, "Split cross-source overlap at its boundaries")
check(local.allSatisfy { $0.speakerId == "local-you" && $0.confidence == 0.8 }, "Keep source identity and confidence")
check(local[1].candidateSpeakerIds == ["remote"], "Keep the remote speaker candidate")
check(crossSource.first { $0.source == "system" }?.candidateSpeakerIds == ["local-you"], "Keep system source separate")

let sameSpeaker = IntervalProcessing.prepare([
    interval("a", 0, 3, source: "microphone"), interval("a", 1, 2),
])
check(sameSpeaker.allSatisfy { !$0.isOverlap && $0.candidateSpeakerIds.isEmpty }, "The same speaker must not count twice")
check(Set(sameSpeaker.map { $0.source }) == Set(["microphone", "system"]), "Keep both audio sources for the same speaker")
let duplicateSpeaker = IntervalProcessing.prepare([interval("a", 0, 3), interval("a", 1, 2)])
check(duplicateSpeaker.count == 1 && !duplicateSpeaker[0].isOverlap, "Duplicate same-source turns must not create overlap")

let normalized = IntervalProcessing.prepare([
    interval("a", 0, 1), interval("a", 1.2, 2), interval("b", 1.5, 1.9),
])
check(normalized.first?.end == 1.5, "Normalize nearby turns before splitting")
check(normalized.first { $0.speakerId == "a" && $0.start == 1.9 }?.isOverlap == false, "Do not merge across an overlap boundary")

let shortSlice = IntervalProcessing.prepare([interval("a", 0, 1), interval("b", 0.9, 2)])
check(shortSlice.filter { $0.isOverlap }.count == 2, "Keep overlap slices shorter than 0.25 seconds")
check(shortSlice.filter { $0.speakerId == "a" }.last?.end == 1, "Keep short boundary slices")

let threeSpeakers = IntervalProcessing.prepare([
    interval("c", 2, 3), interval("a", 0, 5), interval("b", 1, 4),
])
check(threeSpeakers.first { $0.speakerId == "a" && $0.start == 2 }?.candidateSpeakerIds == ["b", "c"], "Sort all active candidates")
check(threeSpeakers.first { $0.speakerId == "a" && $0.start == 3 }?.candidateSpeakerIds == ["b"], "Remove ended candidates")

print("Interval processing: all regression checks passed")
