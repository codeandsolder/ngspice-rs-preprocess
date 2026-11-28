"""Isolated native enhancement-channel operating points, ngspice 47.

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
PTL = ' ptl=0.02 ptlp=0.7 ptp=3.7 pt2=0.12 pt4=0.4 pt4p=0.8'
GDL = ' gdl=0.001 gdld=0.2u gdlp=0.4'
LEAK = ' gdsleak=1e-7'
CORRECTIONS = ('', PTL, GDL, LEAK, PTL+GDL+LEAK)

def main():
    out = Path(sys.argv[1])
    corrections = '--corrections' in sys.argv[2:]
    with tempfile.TemporaryDirectory(prefix='hisim-normal-circuit-') as tmp, out.open('w') as f:
        f.write('# family polarity temp_C length_m width_m multiplicity normalized_Vgs Vds Vbs; ids gm gds gmbs qb qg qd\n')
        if corrections:
            f.write('# additional trailing fields: correction selector, NF\n')
        configurations = product((2,) if corrections else (1,2), (1,-1), (0.,27.,100.),
                                 range(1,5) if corrections else (0,))
        for family, polarity, temp, config in configurations:
            geometry = ((3e-6,10e-6,2.,1.),(10e-6,50e-6,1.,1.))
            if corrections:
                geometry += ((4e-6,30e-6,3.,3.),)
            rows = list(product(geometry, (-2.,0.,1.,2.,3.,5.,8.), (.01,.1,1.,5.), (0.,-.5)))
            card = f'.model xhv {"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=0 coisti=0 coisub=0 coadov=0 coovlp=0 conqs=0'
            if family==2:
                card += ' cordrift=0' + CORRECTIONS[config]
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
                assert all(map(math.isfinite,values)), values
                extra = (config, nf) if corrections else ()
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,vg,vd,vb,*values,*extra)))+'\n')

if __name__ == '__main__':
    main()
