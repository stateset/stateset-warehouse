import { commands } from './commands.mjs';

const labels = { sku: 'SKU', lot: 'Lot', from: 'From bin', to: 'To bin', location: 'Bin', quantity: 'Quantity (each)', counted: 'Observed total (each)', order_id: 'Original order', receipt_id: 'Original receipt', shipment_id: 'Original shipment', transfer_id: 'Original transfer', line_id: 'Original line', hold_id: 'Original hold', exception_id: 'Original exception', expires_at: 'Expiry time', reason: 'Reason', reference: 'Reference' };
function fields(value, prefix = '') {
  if (value === null || typeof value !== 'object') return [`${prefix}: ${value === null ? '(none)' : String(value)}`];
  if (Array.isArray(value)) return value.length ? value.flatMap((item, index) => fields(item, `${prefix} ${index + 1}`.trim())) : [`${prefix}: (none)`];
  return Object.entries(value).flatMap(([key, item]) => fields(item, `${prefix ? `${prefix} / ` : ''}${labels[key] ?? key.replaceAll('_', ' ')}`));
}
export function workSummary(scope, request) {
  const { type, ...command } = request.command;
  const label = commands.find(command => command.type === type)?.label ?? type;
  return [label, '', `Facility: ${scope.warehouse}`, `Actor: ${scope.actor}`, `Tenant: ${scope.tenant}`, `Server: ${scope.origin}`, `Reviewed version: ${request.expected_version}`, `Request: ${request.request_id}`, '', ...fields(command),
    ...(request.unit_inputs?.length ? ['', 'ACTUAL PACK OBSERVATIONS', ...fields(request.unit_inputs, 'Pack')] : []),
    ...(request.serial_inputs?.length ? ['', 'ACTUAL SERIAL OBSERVATIONS', ...fields(request.serial_inputs, 'Target')] : []),
    ...(request.resource_versions ? ['', 'RESOURCE REVIEW', ...fields(request.resource_versions)] : []),
  ].join('\n');
}
export function outcomeSummary(record) {
  const details = record.events.at(-1).details;
  const result = details.response?.result ?? details.receipt?.result;
  return [`Status: ${record.stage}`, `Original request: ${record.request.request_id}`, `Facility: ${record.original.scope.warehouse}`,
    ...(record.stage === 'committed' ? ['The warehouse accepted this original request.', ...(result ? fields(result) : []), ...(details.response?.replayed ? ['Recovered by replaying the exact original request.'] : [])]
      : record.stage === 'not_committed' ? ['The server established that this original request did not commit.'] : ['Outcome is unresolved. Keep the original request.', 'Use original-request recovery before recording more work.']),
  ].join('\n');
}
