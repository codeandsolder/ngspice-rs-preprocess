"""Converged ngspice 47 gate/edge/GIDL/GISL branch current oracle."""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

GATE='gleak1=50 gleak2=1e7 gleak3=0.06 gleak4=4 gleak5=7500 gleak6=0.25 gleak7=1e-6 glkb1=5e-16 glkb2=1 glkb3=0.1 fn1=50 fn2=170e-6 fn3=0 fvbs=0.012 glksd1=2.5e-15 glksd2=1e3 glksd3=-1e3 glpart1=0.35'
GIDL='gidl1=2 gidl2=3e7 gidl3=0.9 gidl4=0.15 gidl5=0.2 lgidl1=0.8 wgidl1=1.2 pgidl1=0.3 lgidl2=2e6 wgidl2=3e6 pgidl2=1e6'
OBSERVABLES=('igd','igs','igb','igidl','igisl')

def main():
    out=Path(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix='hisim-leakage-circuit-') as tmp, out.open('w') as f:
        f.write('# family polarity temp_C length_m width_m multiplicity NF config normalized_Vgs Vds Vbs; igd igs igb igidl igisl\n')
        for family,polarity,temp,config in product((1,2),(1,-1),(27.,100.),range(3)):
            gate_enable=int(config in (0,2))
            gidl_enable=int(config in (1,2))
            rows=list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)),(-2.,0.,1.,2.,5.,8.),(0.,0.01,1.,5.,15.),(0.,-.5)))
            model=f'.model xhv {"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=0 coisti=0 coisub=0 coadov=0 coovlp=0 conqs=0 coiigs={gate_enable} cogidl={gidl_enable} tox=8n vfbc=-0.8 eg0=1.2 egig=0.06 igtemp2=110 igtemp3=1300 '+GATE+' '+GIDL
            if family==2:
                model+=' cordrift=0'
            deck=['* wrapper title','* gate and junction leakage',model,f'.temp {temp}']
            for i,((length,width,m,nf),vg,vd,vb) in enumerate(rows):
                deck.extend((f'Vd{i} d{i} 0 {polarity*vd}', f'Vg{i} g{i} 0 {polarity*vg}', f'Vb{i} b{i} 0 {polarity*vb}', f'M{i} d{i} g{i} 0 b{i} xhv l={length} w={width} m={m} nf={nf}'))
            deck.extend(('.control','set numdgt=16','op'))
            deck.extend('print '+' '.join(f'@m{i}[{name}]' for name in OBSERVABLES) for i in range(len(rows)))
            deck.extend(('quit','.endc','.end'))
            proc=subprocess.run(['ngspice','-b','/dev/stdin'],input='\n'.join(deck)+'\n',text=True,capture_output=True,cwd=tmp,timeout=120)
            if proc.returncode or 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip():
                print(proc.stderr,file=sys.stderr)
            data={(int(i),name):float(value) for i,name,value in re.findall(r'@m(\d+)\[(\w+)\]\s*=\s*([^\s]+)',proc.stdout)}
            assert len(data)==len(rows)*len(OBSERVABLES),(len(data),proc.stdout[-1000:])
            for i,((length,width,m,nf),vg,vd,vb) in enumerate(rows):
                values=[data[i,name] for name in OBSERVABLES]
                assert all(map(math.isfinite,values)),values
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,vg,vd,vb,*values)))+'\n')

if __name__=='__main__':
    main()
