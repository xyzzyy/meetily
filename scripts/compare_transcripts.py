#!/usr/bin/env python3
"""Compare whole transcripts. A machine reference measures agreement, not accuracy."""
import argparse
from array import array
import json
from pathlib import Path
import re
import unicodedata


def load_rows(path):
    data = json.loads(Path(path).read_text(encoding='utf-8'))
    rows = data.get('segments', []) if isinstance(data, dict) else data
    if not isinstance(rows, list):
        raise ValueError('Expected transcript rows or a segments array')
    return sorted(rows, key=lambda r: first_present(r, 'audio_start_time', 'start') or 0)


def first_present(row, *keys):
    # Saved transcripts store missing values as null, not as absent keys.
    return next((row[key] for key in keys if row.get(key) is not None), None)


def words(rows):
    result = []
    for index, row in enumerate(rows):
        text = unicodedata.normalize('NFKC', first_present(row, 'text', 'transcript') or '').lower().replace('’', "'")
        result.extend((word, index) for word in re.findall(r"\w+(?:'\w+)*", text))
    return result


def compare(reference, candidate):
    ref, hyp = words(reference), words(candidate)
    # Keep a byte per cell for the edit path and two score rows. Exact word
    # alignment permits different transcript row boundaries without penalty.
    width = len(hyp) + 1
    path = bytearray((len(ref) + 1) * width)
    path[1:width] = bytes([2]) * len(hyp)
    previous = array('I', range(width))
    for i, (word, _) in enumerate(ref, 1):
        current = array('I', [i])
        path[i * width] = 1
        for j, (other, _) in enumerate(hyp, 1):
            choices = (previous[j - 1] + (word != other), previous[j] + 1, current[j - 1] + 1)
            choice = min(range(3), key=choices.__getitem__)
            current.append(choices[choice])
            path[i * width + j] = choice
        previous = current
    edits = []
    i, j = len(ref), len(hyp)
    while i or j:
        move = path[i * width + j]
        if move == 0:
            i -= 1
            j -= 1
            if ref[i][0] != hyp[j][0]:
                edits.append(('substitution', ref[i][1], ref[i][0], hyp[j][0]))
        elif move == 1:
            i -= 1
            edits.append(('deletion', ref[i][1], ref[i][0], ''))
        else:
            j -= 1
            edits.append(('insertion', ref[max(0, i - 1)][1] if ref else 0, '', hyp[j][0]))
    counts = {kind: sum(e[0] == kind for e in edits) for kind in ['substitution', 'deletion', 'insertion']}
    changes = []
    for index in sorted({e[1] for e in edits}):
        row = reference[index] if reference else {}
        changes.append({
            'reference_row': index,
            'reference_start': first_present(row, 'audio_start_time', 'start'),
            'reference_text': first_present(row, 'text', 'transcript') or '',
            'edits': [{'type': kind, 'before': before, 'after': after}
                      for kind, at, before, after in reversed(edits) if at == index],
        })
    return {'reference_is_ground_truth': False, 'reference_words': len(ref), 'candidate_words': len(hyp),
            'word_difference_rate': len(edits) / len(ref) if ref else None,
            'edits': counts, 'changed_reference_rows': changes}


def compare_with_baseline(reference, baseline, candidate, verified_reference=False):
    """A release gate needs a human-checked reference, not model agreement."""
    if not verified_reference:
        raise ValueError('The regression gate requires a human-checked reference')
    before, after = compare(reference, baseline), compare(reference, candidate)
    if not after['reference_words']:
        raise ValueError('The reference must contain words')
    after['reference_is_ground_truth'] = True
    after['word_error_rate'] = after['word_difference_rate']
    after['baseline_word_error_rate'] = before['word_difference_rate']
    before_regions = {row['reference_row']: len(row['edits']) for row in before['changed_reference_rows']}
    after['regressed_regions'] = [
        {'reference_row': row['reference_row'], 'reference_start': row['reference_start'],
         'reference_text': row['reference_text'], 'baseline_errors': before_regions.get(row['reference_row'], 0),
         'candidate_errors': len(row['edits'])}
        for row in after['changed_reference_rows']
        if len(row['edits']) > before_regions.get(row['reference_row'], 0)
    ]
    after['regression'] = (sum(after['edits'].values()) > sum(before['edits'].values())
                           or bool(after['regressed_regions']))
    return after


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference')
    parser.add_argument('candidate')
    parser.add_argument('--output', required=True)
    parser.add_argument('--baseline', help='Live transcript to test against the same checked reference')
    parser.add_argument('--verified-reference', action='store_true', help='The reference wording has been checked by a person')
    args = parser.parse_args()
    if args.baseline:
        try:
            report = compare_with_baseline(load_rows(args.reference), load_rows(args.baseline),
                                           load_rows(args.candidate), args.verified_reference)
        except ValueError as error:
            parser.error(str(error))
    else:
        report = compare(load_rows(args.reference), load_rows(args.candidate))
        if args.verified_reference:
            report['reference_is_ground_truth'] = True
            report['word_error_rate'] = report['word_difference_rate']
    Path(args.output).write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: v for k, v in report.items() if k != 'changed_reference_rows'}, indent=2))
    return 1 if report.get('regression') else 0


if __name__ == '__main__':
    raise SystemExit(main())
