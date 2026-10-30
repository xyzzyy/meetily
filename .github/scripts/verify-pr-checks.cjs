const { jobs } = require('./select-pr-checks.cjs');
function verify(needs) {
  if (needs.changes?.result !== 'success') throw new Error('Check selection did not succeed');
  const checks = JSON.parse(needs.changes.outputs.checks);
  const failures = jobs.filter(job => {
    if (typeof checks[job] !== 'boolean') throw new Error(`Missing selection for ${job}`);
    return checks[job] && needs[job]?.result !== 'success';
  });
  if (failures.length) throw new Error(`Selected checks did not pass: ${failures.join(', ')}`);
}
module.exports = { verify };
if (require.main === module) {
  verify(JSON.parse(process.env.CHECK_RESULTS));
  console.log('All selected PR checks passed.');
}
