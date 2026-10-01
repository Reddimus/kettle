#!/usr/bin/env python3
"""Fill named publication cells or fact-check the exact rendered document locally."""
import argparse
import importlib.util
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('fill', 'factcheck'))
    parser.add_argument('--template', required=True)
    parser.add_argument('--values', required=True)
    parser.add_argument('--document', required=True, help='new output for fill; existing document for factcheck')
    parser.add_argument('--sessions', nargs='+', help='original session directories, required for both modes')
    parser.add_argument('--aa', help='ordinary control directory, required for both modes')
    parser.add_argument('--without-typing', action='store_true', help='predeclared D1 omission only; check three default spot rows')
    args = parser.parse_args()
    h = load(HERE / 'macos-standing.py', 'standing_fill')
    p = h.publication
    try:
        values = p.strict_json(args.values)
        text = Path(args.template).read_text()
        if not args.sessions or not args.aa:
            parser.error('fill and factcheck require original sessions and shared A/A')
        combined = h.combine([Path(s) for s in args.sessions], Path(args.aa))
        regenerated = p.publication_values(combined)
        if regenerated != values:
            raise ValueError('publication values differ from fresh combine')
        if args.mode == 'factcheck':
            independent = load(HERE / 'macos-standing' / 'publication_factcheck.py', 'independent_facts')
            raw = [p.strict_json(Path(s) / 'results.json') for s in args.sessions]
            independent.verify_estimates(raw, combined)
            independent.spot_rows(raw, require_typing=not args.without_typing)
            if Path(args.document).read_text() != p.fill(text, regenerated):
                raise ValueError('filled document number or caveat mismatch')
            print('factcheck: zero mismatches')
        else:
            rendered = p.fill(text, regenerated)
            with Path(args.document).open('x') as output:
                output.write(rendered)
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, 'publication: ' + str(error) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
