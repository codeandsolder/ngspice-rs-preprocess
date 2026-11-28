"""Isolated native depletion-channel operating points, ngspice 47.

Disable access/drift resistors, extra channel currents and overlap additions.
Use normalized positive drain bias for both polarities. Stored intrinsic
charge and direct channel current observables are read at converged OP.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

OBSERVABLES = ('ids', 'gm', 'gds', 'gmbs', 'qb', 'qg', 'qd')
CARDS = ("", " ndepm=3e17 tndep=0.1u depeta=0.2 depvdsef1=1.5 depvdsef2=0.8 depleak=0.08 depmue0=600 depmue1=40 depmueback0=220 depmueback1=12 depmueph0=0.6 depbb=1.7")

def main():
    out = Path(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix='hisim-normal-circuit-') as tmp, out.open('w') as f:
        f.write('# family polarity temp_C length_m width_m multiplicity normalized_Vgs Vds Vbs; ids gm gds gmbs qb qg qd\n')
        f.write('# trailing fields: depletion card selector, NF\n')
        configurations = product((1,-1), (0.,27.,100.), range(len(CARDS)))
        for polarity,temp,config in configurations:
            family=2
            geometry = ((3e-6,10e-6,2.,1.),(10e-6,50e-6,1.,1.),(4e-6,30e-6,3.,3.))
            rows = list(product(geometry, (-6.,-3.,-1.,0.,1.,3.,5.,8.), (.01,.1,1.,5.), (0.,-.5)))
            card = f'.model xhv {"nmos" if polarity==1 else "pmos"} level=73 version=2.20 codep=1 corsrd=0 coisti=0 coisub=0 coadov=0 coovlp=0 conqs=0 cordrift=0' + CARDS[config]
            deck = ['* wrapper title', '* native normal channel', card, f'.temp {temp}']
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                deck.extend((f'Vd{i} d{i} 0 {polarity*vd}', f'Vg{i} g{i} 0 {polarity*vg}', f'Vb{i} b{i} 0 {polarity*vb}', f'M{i} d{i} g{i} 0 b{i} xhv l={length} w={width} m={m} nf={nf}'))
            deck.extend(('.control','set numdgt=16','op'))
            deck.extend('print '+ ' '.join(f'@m{i}[{name}]' for name in OBSERVABLES) for i in range(len(rows)))
            deck.extend(('quit','.endc','.end'))
            proc = subprocess.run(['ngspice','-b','/dev/stdin'], input='\n'.join(deck)+'\n', text=True, capture_output=True, cwd=tmp, check=True, timeout=120)
            if 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip():
                print(proc.stderr,file=sys.stderr)
            data = {(int(i),name):float(value) for i,name,value in re.findall(r'@m(\d+)\[(\w+)\]\s*=\s*([^\s]+)',proc.stdout)}
            assert len(data)==len(rows)*len(OBSERVABLES), (len(data),proc.stdout[-1000:])
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                values=[data[i,name] for name in OBSERVABLES]
                if not all(map(math.isfinite,values)):
                    print("native nonfinite",polarity,temp,config,length,width,m,nf,vg,vd,vb,values,file=sys.stderr)
                    continue
                extra = (config, nf)
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,vg,vd,vb,*values,*extra)))+'\n')

if __name__ == '__main__':
    main()
