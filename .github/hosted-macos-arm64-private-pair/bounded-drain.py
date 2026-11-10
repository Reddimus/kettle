import hashlib
import json
import os
import sys
import time
from pathlib import Path
root=Path(os.environ["CAPTURE_ROOT"])
limit=16*1024*1024
total=kept=echoed=0
live_limit=64*1024
digest=hashlib.sha256()
with (root/"strict.log").open("xb") as output:
    while True:
        data=os.read(sys.stdin.fileno(),65536)
        if not data:
            break
        total+=len(data)
        digest.update(data)
        retained=data[:max(0,limit-kept)]
        output.write(retained)
        output.flush()
        kept+=len(retained)
        progress=json.dumps({"strict_log_bytes":total,"last_chunk_bytes":len(data),"wall_ns":time.time_ns()},sort_keys=True)+"\n"
        progress_bytes=len(progress.encode("ascii"))
        if echoed+progress_bytes<=live_limit:
            sys.stdout.write(progress)
            sys.stdout.flush()
            echoed+=progress_bytes
record={"bytes":total,"retained_bytes":kept,"sha256_all_drained_bytes":digest.hexdigest(),"limit":limit,"over_limit":total>limit,"pipe_eof":True,"drained_wall_ns":time.time_ns(),"live_echo_bytes":echoed,"live_echo_limit":live_limit}
(root/"strict-log.json").write_text(json.dumps(record,indent=2)+"\n")
print(json.dumps(record,sort_keys=True))
raise SystemExit(2 if total>limit else 0)
