import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFileSync } from "node:fs";
import { promisify } from "node:util";
import test from "node:test";

const run = promisify(execFile);
const helper = new URL("./e2e/process.mjs", import.meta.url).href;

for (const parentExits of [false, true]) {
  test(`GUI cleanup kills pipe-holding descendants when the driver ${parentExits ? "already exited" : "is running"}`, { skip: process.platform !== "linux" }, async () => {
    const descendant = `process.on('SIGTERM',()=>{}); process.stderr.write('ready'); setInterval(()=>{},1000);`;
    const parent = `
      const {spawn}=require('node:child_process');
      const child=spawn(process.execPath,['-e',${JSON.stringify(descendant)}],{stdio:['ignore','ignore','inherit']});
      process.stdout.write(String(child.pid)); child.unref();
      ${parentExits ? "process.exit(0);" : "setInterval(()=>{},1000);"}
    `;
    const harness = `
      import {once} from 'node:events';
      import {startProcess} from ${JSON.stringify(helper)};
      const {child,stop}=startProcess(process.execPath,['-e',${JSON.stringify(parent)}],{stdio:['ignore','pipe','pipe']});
      console.log('group:'+child.pid);
      child.stdout.on('data',data=>console.log('descendant:'+data));
      const exited=once(child,'exit');
      await once(child.stderr,'data');
      ${parentExits ? "await exited;" : ""}
      await stop(100);
      console.log('cleaned');
    `;
    let output = "";
    try {
      ({ stdout: output } = await run(process.execPath, ["--input-type=module", "-e", harness], { timeout: 5000 }));
      assert.match(output, /cleaned/);
      const pid = Number(output.match(/descendant:(\d+)/)?.[1]);
      assert.ok(pid > 0);
      try {
        const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
        assert.equal(stat.slice(stat.lastIndexOf(")") + 2)[0], "Z", "descendant is still running");
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    } catch (error) {
      output = error.stdout ?? output;
      throw error;
    } finally {
      const group = Number(output.match(/group:(\d+)/)?.[1]);
      if (group > 0) {
        try { process.kill(-group, "SIGKILL"); }
        catch (error) { if (error.code !== "ESRCH") throw error; }
      }
    }
  });
}
