ObjC.import('AppKit');
ObjC.import('Foundation');

function run() {
    const input = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
    const request = JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(input, $.NSUTF8StringEncoding)));
    const board = request.name ? $.NSPasteboard.pasteboardWithName(request.name) : $.NSPasteboard.generalPasteboard;
    function skip(reason) { return JSON.stringify({ok: false, reason: reason}); }
    if (!board || board.isNil()) return skip('pasteboard service unavailable');
    const count = Number(board.changeCount);
    if (request.op === 'snapshot') {
        const items = board.pasteboardItems;
        const saved = [];
        let bytes = 0;
        if (items.count > 128) return skip('too many pasteboard items');
        for (let i = 0; i < items.count; i++) {
            const item = items.objectAtIndex(i);
            const types = item.types;
            const savedTypes = [];
            if (types.count > 128) return skip('too many pasteboard types');
            for (let j = 0; j < types.count; j++) {
                const type = types.objectAtIndex(j);
                const data = item.dataForType(type);
                if (!data || data.isNil()) return skip('a pasteboard type cannot be captured');
                bytes += Number(data.length);
                if (bytes > 16 * 1024 * 1024) return skip('pasteboard exceeds snapshot limit');
                savedTypes.push([ObjC.unwrap(type), ObjC.unwrap(data.base64EncodedStringWithOptions(0))]);
            }
            if (!savedTypes.length) return skip('an item has no restorable types');
            saved.push(savedTypes);
        }
        if (Number(board.changeCount) !== count) return skip('pasteboard changed during snapshot');
        return JSON.stringify({ok: true, count: count, items: saved});
    }
    const snapshot = request.snapshot;
    if (request.op === 'restore' && count === snapshot.count) return JSON.stringify({ok: true});
    // Kettle clears once before writing its string. Any other generation is foreign.
    if (count !== snapshot.count + 1 || ObjC.unwrap(board.stringForType('public.utf8-plain-text')) !== request.text)
        return skip('pasteboard changed outside the clipboard probe');
    if (request.op === 'check') return JSON.stringify({ok: true});
    const items = $.NSMutableArray.alloc.init;
    for (const types of snapshot.items) {
        const item = $.NSPasteboardItem.alloc.init;
        for (const entry of types) {
            const data = $.NSData.alloc.initWithBase64EncodedStringOptions(entry[1], 0);
            if (!item.setDataForType(data, entry[0])) return skip('cannot prepare pasteboard restore');
        }
        items.addObject(item);
    }
    if (Number(board.changeCount) !== count) return skip('pasteboard changed before restore');
    board.clearContents;
    if (items.count && !board.writeObjects(items)) throw Error('pasteboard restore failed');
    return JSON.stringify({ok: true});
}
