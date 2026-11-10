from pathlib import Path
import sys, re, json, hashlib, subprocess, tomllib, datetime

root = Path.cwd()
out = root / 'target/companion-final/dpi-vendor-2026-11-09'
paths = ['Cargo.toml', 'Cargo.lock', 'third_party/gpui/Cargo.toml']

def sha(raw):
    return hashlib.sha256(raw).hexdigest()

def source_files():
    scopes = ['.cargo', 'Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'crates', 'scripts', 'third_party/gpui']
    return [x.decode() for x in subprocess.check_output(['git', 'ls-files', '-z', '--', *scopes]).split(b'\0') if x]

def identities(raw):
    return sorted((p['name'], p['version'], p.get('source'), p.get('checksum'))
                  for p in tomllib.loads(raw.decode())['package'])

phase = sys.argv[1]
if phase == 'prepare':
    assert not (out/'prepared.json').exists(), 'Refuse to replace harness backups'
    before = {p: (root/p).read_bytes() for p in paths}
    for i, p in enumerate(paths):
        (out/f'{i}.before').write_bytes(before[p])
    files = [{'path': p, 'sha256': sha((root/p).read_bytes())} for p in source_files()]
    (out/'source-before.json').write_text(json.dumps(files, indent=2)+'\n')
    manifest = before['Cargo.toml']
    newline = b'\r\n' if b'\r\n' in manifest else b'\n'
    marker = b'members = [' + newline
    assert manifest.count(marker) == 1
    assert b'"third_party/gpui",' not in manifest
    manifest = manifest.replace(marker, marker + b'    "third_party/gpui",' + newline, 1)
    vendor = before['third_party/gpui/Cargo.toml'].decode()
    for name in ['env_logger', 'lyon', 'pretty_assertions']:
        pattern = rf'(?ms)^\[dev-dependencies\.{name}\]\r?\n.*?(?=^\[|\Z)'
        vendor, count = re.subn(pattern, '', vendor)
        assert count == 1, name
    (root/'Cargo.toml').write_bytes(manifest)
    (root/'third_party/gpui/Cargo.toml').write_bytes(vendor.encode())
    info = {'sourceHead': subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
            'createdAtUtc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'backups': [{'path':p,'backup':f'{i}.before','sha256':sha(before[p])} for i,p in enumerate(paths)],
            'packageIdentitiesBefore':len(identities(before['Cargo.lock'])),
            'omittedUnusedDevDependencies':['env_logger','lyon extra','pretty_assertions']}
    (out/'prepared.json').write_text(json.dumps(info,indent=2)+'\n')
    print(json.dumps({'prepared':True,'sourceFiles':len(files),'packageIdentities':info['packageIdentitiesBefore']}))
elif phase == 'verify':
    old = identities((out/'1.before').read_bytes())
    new = identities((root/'Cargo.lock').read_bytes())
    removed = [x for x in old if x not in new]
    added = [x for x in new if x not in old]
    report = {'samePackageIdentities':old==new,'before':len(old),'after':len(new),'added':added,'removed':removed}
    (out/'lock-proof.json').write_text(json.dumps(report,indent=2)+'\n')
    for i,p in enumerate(paths):
        (out/f'{i}.temporary').write_bytes((root/p).read_bytes())
    (out/'temporary-harness.patch').write_bytes(subprocess.check_output(['git','diff','--',*paths]))
    print(json.dumps(report))
    assert old == new, 'Package graph identities changed: restore before proceeding'
elif phase == 'restore':
    info = json.loads((out/'prepared.json').read_text())
    restored=[]
    for row in info['backups']:
        raw=(out/row['backup']).read_bytes();assert sha(raw)==row['sha256']
        (root/row['path']).write_bytes(raw)
        assert (root/row['path']).read_bytes()==raw
        restored.append({'path':row['path'],'sha256':sha(raw),'byteExact':True})
    before=json.loads((out/'source-before.json').read_text())
    drift=[row['path'] for row in before if sha((root/row['path']).read_bytes())!=row['sha256']]
    report={'restored':restored,'sourceFilesVerified':len(before),'sourceDrift':drift}
    (out/'restoration.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report))
    assert not drift, drift
else:
    raise SystemExit('Unknown phase')
