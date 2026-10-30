import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('comparison', Path(__file__).parents[1] / 'compare_transcripts.py')
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)


class ComparisonTests(unittest.TestCase):
    def test_row_boundaries_and_punctuation_do_not_count_as_errors(self):
        result = comparison.compare([{'text': "Andrew, feel free to drop… or stay."}],
                                    [{'text': 'Andrew feel free'}, {'text': 'to drop or stay'}])
        self.assertEqual(result['word_difference_rate'], 0)

    def test_name_error_and_lost_continuation_are_separate(self):
        result = comparison.compare([{'text': 'Andrew feel free to drop or stay', 'start': 643}],
                                    [{'text': 'Enter feel free to drop'}])
        self.assertEqual(result['edits'], {'substitution': 1, 'deletion': 2, 'insertion': 0})
        self.assertEqual(result['changed_reference_rows'][0]['reference_start'], 643)
        self.assertFalse(result['reference_is_ground_truth'])

    def test_added_words_and_empty_transcripts(self):
        result = comparison.compare([{'text': 'thank you'}], [{'text': 'thank thank you'}])
        self.assertEqual(result['edits']['insertion'], 1)
        self.assertEqual(comparison.compare([{'text': 'hello'}], [])['edits']['deletion'], 1)
        self.assertEqual(comparison.compare([], [{'text': 'hello'}])['edits']['insertion'], 1)

    def test_null_times_and_text_from_saved_transcripts(self):
        rows = [{'transcript': 'stay', 'audio_start_time': 5.0},
                {'transcript': 'Andrew', 'audio_start_time': None, 'text': None}]
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        path = Path(folder.name) / 'rows.json'
        path.write_text(json.dumps(rows))
        loaded = comparison.load_rows(path)
        self.assertEqual([row['transcript'] for row in loaded], ['Andrew', 'stay'])
        self.assertEqual(comparison.compare([{'text': 'Andrew stay'}], loaded)['word_difference_rate'], 0)

    def test_regression_gate_requires_checked_words(self):
        with self.assertRaisesRegex(ValueError, 'human-checked'):
            comparison.compare_with_baseline([{'text': 'Andrew'}], [{'text': 'Andrew'}], [{'text': 'Enter'}])

    def test_gate_detects_a_whole_transcript_regression(self):
        gold = [{'text': 'Andrew feel free to drop or stay'}, {'text': 'The flow that you had built'}]
        baseline = [{'text': 'Andrew feel free to drop or stay The flow that you had built'}]
        candidate = [{'text': 'And feel free to drop or stay'}]
        result = comparison.compare_with_baseline(gold, baseline, candidate, True)
        self.assertTrue(result['regression'])
        self.assertGreater(result['word_error_rate'], result['baseline_word_error_rate'])

    def test_gate_accepts_an_improvement_and_unchanged_accuracy(self):
        gold = [{'text': 'Andrew feel free to drop or stay'}]
        baseline = [{'text': 'I feel very true Trump'}]
        candidate = [{'text': 'And feel free to drop or stay'}]
        result = comparison.compare_with_baseline(gold, baseline, candidate, True)
        self.assertFalse(result['regression'])
        self.assertLess(result['word_error_rate'], result['baseline_word_error_rate'])
        self.assertFalse(comparison.compare_with_baseline(gold, gold, gold, True)['regression'])


    def test_overall_improvement_cannot_hide_a_worse_section(self):
        gold = [{'text': 'one two three four five', 'start': 0}, {'text': 'Do not delete data', 'start': 10}]
        baseline = [{'text': 'bad bad bad bad bad Do not delete data'}]
        candidate = [{'text': 'one two three four five Do delete data'}]
        result = comparison.compare_with_baseline(gold, baseline, candidate, True)
        self.assertLess(result['word_error_rate'], result['baseline_word_error_rate'])
        self.assertTrue(result['regression'])
        self.assertEqual(result['regressed_regions'][0]['reference_start'], 10)



if __name__ == '__main__':
    unittest.main()
