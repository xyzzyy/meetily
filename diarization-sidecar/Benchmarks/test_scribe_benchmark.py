import unittest, itertools, random
from scribe_benchmark import best_mapping,compare
class BenchmarkTests(unittest.TestCase):
 def test_label_permutation_is_not_an_error(self):
  ref={'words':[{'type':'word','start':0,'end':2,'speaker_id':'alice'},{'type':'word','start':3,'end':5,'speaker_id':'bob'}]}
  hyp={'intervals':[{'start':0,'end':2,'speakerId':'S9'},{'start':3,'end':5,'speakerId':'S2'}]}
  r=compare(ref,hyp)
  self.assertEqual(r['agreement_on_all_reference_speech'],1)
  self.assertEqual(r['speaker_mapping'],{'S9':'alice','S2':'bob'})
 def test_collapsed_speakers_are_penalized(self):
  ref={'words':[{'type':'word','start':i,'end':i+1,'speaker_id':str(i)} for i in range(8)]}
  hyp={'intervals':[{'start':0,'end':8,'speakerId':'S1'}]}
  self.assertEqual(compare(ref,hyp)['agreement_on_all_reference_speech'],.125)
 def test_missing_and_overlap_are_separate(self):
  ref={'words':[{'type':'word','start':0,'end':4,'speaker_id':'A'}]}
  hyp={'intervals':[{'start':0,'end':2,'speakerId':'S1'},{'start':1,'end':3,'speakerId':'S2'}]}
  r=compare(ref,hyp)
  self.assertEqual(r['missing_seconds'],1)
  self.assertEqual(r['local_overlap_seconds'],1)
  self.assertEqual(r['single_label_coverage'],.5)
  self.assertEqual(r['agreement_on_all_reference_speech'],.25)
 def test_optimal_mapping_matches_brute_force(self):
  rng=random.Random(13)
  for nr,nh in [(1,3),(3,1),(3,3),(4,3)]:
   rows=[f'h{i}' for i in range(nh)];cols=[f'r{i}' for i in range(nr)]
   weights={(r,c):rng.random() for r in rows for c in cols}
   _,score=best_mapping(weights,rows,cols)
   n=max(nr,nh)
   expected=max(sum(weights.get((rows[i],cols[p[i]]),0) if i<nh and p[i]<nr else 0 for i in range(n)) for p in itertools.permutations(range(n)))
   self.assertAlmostEqual(score,expected)
if __name__=='__main__': unittest.main()
