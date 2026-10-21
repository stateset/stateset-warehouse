#!/usr/bin/env python3
"""Record host observations and reject insufficient qualification storage."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import shutil


def host_snapshot(paths, proc_root=Path('/proc')):
    filesystems = {}
    for label, path in paths.items():
        try:
            usage = shutil.disk_usage(path)
            filesystems[label] = {'total_bytes': usage.total, 'used_bytes': usage.used,
                                  'available_bytes': usage.free}
        except OSError as error:
            filesystems[label] = {'error': type(error).__name__}
    pressure = {}
    for name in ('cpu', 'io', 'memory'):
        try:
            pressure[name] = (proc_root / 'pressure' / name).read_text()[:4096]
        except OSError:
            pressure[name] = None
    available_memory = None
    try:
        match = re.search(r'^MemAvailable:\s+(\d+) kB$',
                          (proc_root / 'meminfo').read_text(), re.M)
        if match:
            available_memory = int(match[1]) * 1024
    except OSError:
        pass
    return {'schema_version': 1, 'recorded_at': datetime.now(timezone.utc).isoformat(),
            'filesystems': filesystems, 'linux_pressure': pressure,
            'available_memory_bytes': available_memory,
            'scope': 'Local process host observations; no environment, credentials or process inventory',
            'limit': 'Observations do not establish failure causation, Docker daemon storage capacity or production readiness'}


def storage_preflight(paths, minimum_free_gib, proc_root=Path('/proc')):
    if minimum_free_gib < 1:
        raise ValueError('Minimum free GiB must be a positive integer')
    report = host_snapshot(paths, proc_root)
    required = minimum_free_gib * 1024 ** 3
    reasons = []
    for label, filesystem in report['filesystems'].items():
        if 'error' in filesystem:
            reasons.append(label + ': available storage could not be inspected')
        elif filesystem['available_bytes'] < required:
            reasons.append(label + ': insufficient available storage')
    report.update(ok=not reasons, minimum_free_gib=minimum_free_gib,
                  required_available_bytes=required, reasons=reasons,
                  storage_policy='Headroom floor, not a complete build/fixture capacity estimate')
    return report


def write_report(path, report):
    # Never replace an earlier observation, including an earlier failure.
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as stream:
        json.dump(report, stream, indent=2)
        stream.write('\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--path', action='append', required=True, metavar='LABEL=PATH')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--minimum-free-gib', type=int,
                        default=os.environ.get('WMS_QUALIFICATION_MIN_FREE_GIB', '5'))
    args = parser.parse_args()
    paths = {}
    for value in args.path:
        label, separator, path = value.partition('=')
        if not separator or not path or not re.fullmatch(r'[a-z0-9_-]{1,32}', label) or label in paths:
            parser.error('Paths require distinct LABEL=PATH values with simple lowercase labels')
        paths[label] = Path(path)
    if args.minimum_free_gib < 1:
        parser.error('Minimum free GiB must be a positive integer')
    report = storage_preflight(paths, args.minimum_free_gib)
    write_report(args.output, report)
    print(json.dumps({'ok': report['ok'], 'minimum_free_gib': args.minimum_free_gib,
                      'reasons': report['reasons']}))
    raise SystemExit(0 if report['ok'] else 1)


if __name__ == '__main__':
    main()
