// Run in a fresh Node process: extra roots are captured before application code.
import {getCACertificates,rootCertificates} from 'node:tls';
import {unlinkSync,readFileSync} from 'node:fs';
import assert from 'node:assert/strict';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {fetchWithAgyCliTransport}=await import(pathToFileURL(join(process.env.RPI_REFERENCE_DIR,'packages/core/dist/agy-transport.js')));
assert.equal(process.version,'v24.22.0');
assert.equal(readFileSync(new URL('../data/node24-roots.pem',import.meta.url),'utf8'),rootCertificates.join('\n')+'\n');
getCACertificates('default');
if(process.env.RPI_TRUST_DELETE==='1')unlinkSync(process.env.NODE_EXTRA_CA_CERTS);
const url=process.env.RPI_TRUST_TEST_URL;
const results=[];
for(const agy of [true,false]) {
 try {
  const response=agy?await fetchWithAgyCliTransport(url,{body:'x'},{timeoutMs:2000}):await fetch(url,{method:'POST',body:'x',signal:AbortSignal.timeout(2000)});
  results.push(response.status===200 && await response.text()==='trusted');
 } catch {results.push(false);}
}
console.log('TRUST_RESULT='+JSON.stringify(results));
