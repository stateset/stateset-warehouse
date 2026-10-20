import { safeText } from './client.mjs';

// A previous session identifies local recovery files only. It never substitutes
// for a fresh authenticated session or permits work during an outage.
export async function connectSession(client, ui, { previous, inspectRecovery } = {}) {
  for (;;) {
    let problem;
    ui.screen('Connecting to WMS');
    ui.line(client.origin);
    ui.line('Verifying live operator identity…', 'muted');
    try {
      const session = await client.session();
      if (previous && ['tenant', 'actor'].some(key => session.identity[key] !== previous.identity[key])) {
        throw new Error('Authenticated identity changed. Restore the original credential or exit and restart for the new identity.');
      }
      return session;
    } catch (error) { problem = safeText(error.message); }
    let recoveryProblem = '';
    for (;;) {
      const intro = [
        'ACTION: Session held · r retry connection',
        `Server: ${client.origin}`,
        ...(previous ? [`Last verified operator: ${previous.identity.actor}`, `Last verified tenant: ${previous.identity.tenant}`] : ['No authenticated identity verified yet.']),
        `Read held: ${problem}`,
        'Retry verifies the live identity. No work is sent automatically.',
        ...(inspectRecovery ? ['c inspects saved original requests locally; receipt lookup and resend wait for a verified connection.'] : []),
        ...(recoveryProblem ? [`Recovery check held: ${recoveryProblem}`] : []),
      ];
      const action = await ui.menu('Connection held', [
        ['Retry authenticated connection', 'retry'],
        ...(inspectRecovery ? [['Inspect local original requests', 'recovery']] : []),
        ['Exit console safely', 'exit'],
      ], {
        intro, compactIntro: intro.slice(0, 4),
        compactHint: [`r retry${inspectRecovery ? ' · c local recovery' : ''} · q exit`, 'v full connection details'],
        shortcuts: { r: 'retry', ...(inspectRecovery ? { c: 'recovery' } : {}) },
      });
      if (action === 'retry') break;
      if (action !== 'recovery' || !inspectRecovery) return null;
      try { await inspectRecovery(); recoveryProblem = ''; }
      catch (error) { recoveryProblem = safeText(error.message); }
    }
  }
}
