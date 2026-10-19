#!/usr/bin/env python3
"""Local Scribe v2 reference and speaker-label comparison. Never logs credentials."""
import argparse, collections, hashlib, json, math, os, sys, time, urllib.request, urllib.error, uuid
from pathlib import Path


def save_json(path, data):
    temp = path.with_suffix(path.suffix + '.tmp')
    temp.write_text(json.dumps(data, indent=2))
    os.chmod(temp, 0o600)
    temp.replace(path)


def read_key(path):
    key = os.environ.get('ELEVENLABS_API_KEY', '')
    if not key and path is not None and path.exists():
        for line in path.read_text().splitlines():
            name, sep, value = line.strip().partition('=')
            if sep and name.strip() == 'ELEVENLABS_API_KEY':
                key = value.strip().strip('\"\'')
                break
    if not key:
        raise ValueError('No API key configured. Fill the private key file first.')
    return key


def transcribe(audio, out, key_file):
    if out.exists():
        raise ValueError('Reference already exists; keeping it to avoid a duplicate paid request.')
    key = read_key(key_file)
    boundary = 'meetily-' + uuid.uuid4().hex
    fields = {'model_id': 'scribe_v2', 'diarize': 'true', 'timestamps_granularity': 'word',
              'tag_audio_events': 'false', 'temperature': '0', 'seed': '0'}
    data = bytearray()
    for name, value in fields.items():
        data.extend(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode())
    data.extend(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="benchmark-audio.m4a"\r\nContent-Type: audio/mp4\r\n\r\n'.encode())
    audio_bytes = audio.read_bytes()
    data.extend(audio_bytes)
    data.extend(f'\r\n--{boundary}--\r\n'.encode())
    request = urllib.request.Request('https://api.elevenlabs.io/v1/speech-to-text', data=bytes(data),
        headers={'xi-api-key': key, 'Content-Type': f'multipart/form-data; boundary={boundary}',
                 'Accept': 'application/json'}, method='POST')
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=600) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        # Do not print request headers or a server response that might echo a key.
        raise ValueError(f'ElevenLabs returned HTTP {error.code}; no retry was sent.') from None
    if not isinstance(result.get('words'), list):
        raise ValueError('Scribe returned no word timestamps; the response was not accepted as a benchmark.')
    save_json(out, result)
    save_json(out.with_suffix('.metadata.json'), {
        'model_id': fields['model_id'], 'parameters': fields,
        'audio_sha256': hashlib.sha256(audio_bytes).hexdigest(),
        'elapsed_seconds': round(time.monotonic() - started, 2),
        'reference_is_machine_generated': True})
    print(f'Saved Scribe reference: {out}', flush=True)


def best_mapping(weights, predicted, reference):
    """Maximum-duration one-to-one label assignment (Hungarian algorithm)."""
    n = max(len(predicted), len(reference))
    if not n:
        return {}, 0.0
    matrix = [[weights.get((predicted[i], reference[j]), 0.0)
               if i < len(predicted) and j < len(reference) else 0.0
               for j in range(n)] for i in range(n)]
    largest = max(map(max, matrix), default=0.0)
    costs = [[largest - value for value in row] for row in matrix]
    u, v, p, way = [0.0]*(n+1), [0.0]*(n+1), [0]*(n+1), [0]*(n+1)
    for i in range(1, n+1):
        p[0] = i
        j0 = 0
        minimum = [math.inf]*(n+1)
        used = [False]*(n+1)
        while True:
            used[j0] = True
            i0 = p[j0]
            delta, j1 = math.inf, 0
            for j in range(1, n+1):
                if not used[j]:
                    current = costs[i0-1][j-1] - u[i0] - v[j]
                    if current < minimum[j]:
                        minimum[j], way[j] = current, j0
                    if minimum[j] < delta:
                        delta, j1 = minimum[j], j
            for j in range(n+1):
                if used[j]:
                    u[p[j]] += delta
                    v[j] -= delta
                else:
                    minimum[j] -= delta
            j0 = j1
            if p[j0] == 0:
                break
        while True:
            j1 = way[j0]
            p[j0] = p[j1]
            j0 = j1
            if j0 == 0:
                break
    mapping, score = {}, 0.0
    for j in range(1, n+1):
        i = p[j]-1
        if i < len(predicted) and j-1 < len(reference):
            mapping[predicted[i]] = reference[j-1]
            score += matrix[i][j-1]
    return mapping, score


def compare(reference, hypothesis):
    """Score only Scribe word time, with missing and overlapping labels separate.

    This is agreement with a machine reference, not human-verified DER. Silence
    between words is not treated as reference speech. No word text is exported.
    """
    events = collections.defaultdict(list)
    ref_ids, hyp_ids = set(), set()
    for word in reference.get('words', []):
        if word.get('type') != 'word' or word.get('speaker_id') is None:
            continue
        start, end, speaker = word.get('start'), word.get('end'), word['speaker_id']
        if start is None or end is None or not math.isfinite(start+end) or end <= start:
            continue
        ref_ids.add(speaker)
        events[start].append(('r', speaker, 1)); events[end].append(('r', speaker, -1))
    for segment in hypothesis.get('intervals', []):
        start, end = segment['start'], segment['end']
        if not math.isfinite(start+end) or end <= start:
            continue
        # An overlap label itself cannot be assigned to one person. Keep all
        # candidate voices active so this time is counted as ambiguous.
        speakers = [segment['speakerId']]
        if segment.get('isOverlap'):
            speakers += segment.get('candidateSpeakerIds') or []
        for speaker in set(speakers):
            hyp_ids.add(speaker)
            events[start].append(('h', speaker, 1)); events[end].append(('h', speaker, -1))
    active_r, active_h = collections.Counter(), collections.Counter()
    weights = collections.Counter()
    ref_total = comparable = missing = ambiguous_h = ambiguous_r = 0.0
    intervals = []
    previous = None
    for point in sorted(events):
        if previous is not None and point > previous and active_r:
            duration = point-previous
            if len(active_r) == 1:
                ref_total += duration
                rs = next(iter(active_r))
                if len(active_h) == 1:
                    hs = next(iter(active_h))
                    comparable += duration
                    weights[(hs, rs)] += duration
                    intervals.append((previous, point, hs, rs))
                elif active_h:
                    ambiguous_h += duration
                else:
                    missing += duration
            else:
                ambiguous_r += duration
        for kind, speaker, delta in events[point]:
            active = active_r if kind == 'r' else active_h
            active[speaker] += delta
            if active[speaker] == 0:
                del active[speaker]
        previous = point
    mapping, correct = best_mapping(weights, sorted(hyp_ids), sorted(ref_ids))
    disagreements = []
    for start, end, hs, rs in intervals:
        if mapping.get(hs) == rs:
            continue
        if disagreements and start <= disagreements[-1]['end'] + .35 and \
                disagreements[-1]['local_speaker'] == hs and disagreements[-1]['reference_speaker'] == rs:
            disagreements[-1]['end'] = end
            disagreements[-1]['speech_seconds'] += end-start
        else:
            disagreements.append({'start':start, 'end':end, 'local_speaker':hs,
                'reference_speaker':rs, 'speech_seconds':end-start})
    return {
        'metric': 'time-weighted speaker agreement on Scribe word spans; not verified DER',
        'reference_speakers': len(ref_ids), 'local_speakers': len(hyp_ids),
        'reference_speech_seconds': round(ref_total,3),
        'single_label_coverage': round(comparable/ref_total,4) if ref_total else None,
        'agreement_on_comparable_speech': round(correct/comparable,4) if comparable else None,
        'agreement_on_all_reference_speech': round(correct/ref_total,4) if ref_total else None,
        'missing_seconds':round(missing,3), 'local_overlap_seconds':round(ambiguous_h,3),
        'excluded_reference_overlap_seconds':round(ambiguous_r,3),
        'speaker_mapping':mapping,
        'confusion_seconds':{a:{b:round(weights[(a,b)],3) for b in sorted(ref_ids)} for a in sorted(hyp_ids)},
        'largest_disagreements':sorted(disagreements,key=lambda s:s['speech_seconds'],reverse=True)[:20]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--transcribe', action='store_true')
    parser.add_argument('--audio', type=Path)
    parser.add_argument('--key-file', type=Path)
    parser.add_argument('--reference', type=Path, required=True)
    parser.add_argument('--hypothesis', action='append', default=[], metavar='LABEL=JSON')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    if args.transcribe:
        if args.audio is None:
            parser.error('--audio is required with --transcribe')
        transcribe(args.audio, args.reference, args.key_file)
    if not args.reference.exists():
        raise ValueError('The Scribe reference is not available yet.')
    if not args.hypothesis:
        return
    reference = json.loads(args.reference.read_text())
    report = {}
    for item in args.hypothesis:
        name, separator, file = item.partition('=')
        if not separator or not name or not file:
            parser.error('--hypothesis must be LABEL=JSON')
        hypothesis = json.loads(Path(file).read_text())
        if not hypothesis.get('ok'):
            raise ValueError(f'The local run {name} failed; no score was calculated.')
        report[name] = compare(reference, hypothesis)
    output = args.report or args.reference.with_name('speaker-comparison.json')
    if output.resolve() == args.reference.resolve():
        raise ValueError('Report path must differ from the reference path.')
    save_json(output, report)
    for name, result in report.items():
        print(name, json.dumps({k: v for k, v in result.items() if k in [
            'reference_speakers', 'local_speakers', 'single_label_coverage',
            'agreement_on_comparable_speech', 'agreement_on_all_reference_speech']}))

if __name__=='__main__':
    try:
        main()
    except (ValueError, OSError) as error:
        print(str(error),file=sys.stderr)
        sys.exit(1)
