"""Native CLM and mobility block oracle, extracted from pinned ngspice 47.

Evaluates the native constitutive blocks at independent local channel states,
then finite-differences their complete value path in extended precision.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

from hisim_hv_surface_reference import REVISION

NAMES = ('Vds', 'Ps0', 'Psl', 'Vbscl', 'Qn0', 'Qbu', 'Qiu', 'Idd', 'beta',
         'FMDVDS', 'FD2', 'Leff', 'nsub', 'clm1', 'clm2', 'clm3', 'clmmod',
         'ndep_o_esi', 'ninv_o_esi', 'ninvd', 'muecb0', 'muecb1', 'mueph0',
         'muesr', 'muesr1', 'mphn0', 'Vmax', 'bb', 'weff_nf', 'multiplicity')

# Perturb the actual local constitutive inputs, not model coefficients.
VARIABLES = tuple(range(11)) + (19, 25, 26)


def program(root, version):
    prefix = 'hsmhv' if version == 1 else 'hsmhv2'
    src = subprocess.check_output(['git', '-C', str(root), 'show',
        f'{REVISION}:src/spicelib/devices/hisimhv{version}/{prefix}eval.c'], text=True)
    start = src.index('if( pParam->', src.index('Channel Length Modulation. Lred:'))
    end = src.index('Qbu : -Qb in unit area.', start)
    clm = src[start:src.rfind('/*', start, end)]
    start = src.index('Lch = Leff - Lred', src.index('start_of_mobility:'))
    end = src.index('Ids0_dT =', start)
    mob = src[start:src.index(';', end)+1]
    # Remove diagnostics (not the branch), retain every source value expression.
    sections = re.sub(r'fprintf\s*\([^;]+;', '', clm + '\n' + mob)
    sections = re.sub(r'/\*.*?\*/', '', sections, flags=re.S)
    sections = re.sub(r'(?:here|pParam|model)->HSMHV2?_(\w+)', r'\1', sections)
    # Native derivative assignments are evaluated with zero derivative inputs;
    # our reference derivatives come from independently perturbed values.
    derivs = set(re.findall(r'\b\w+_d(?:V\w+|T)\b', sections))
    decl = ';'.join(f'long double {v}=0.' for v in sorted(derivs)) + ';'
    lhs = set(re.findall(r'\b([A-Za-z_]\w*)\s*(?:=|\*=|/=|\+=|-=)\s*[^=]', sections))
    lhs -= derivs | set(NAMES)
    lhs.update(('TY', 'T0', 'T1', 'T2', 'T3', 'T4', 'T5', 'T6', 'T7', 'T8', 'T9', 'T10', 'T11'))
    lhs -= {'Lred', 'Psdl', 'Lch', 'Eeff', 'Muun', 'Ey', 'Mu', 'Ids0'}
    scalars = ','.join(f'{v}=0.' for v in sorted(lhs))
    # Split the two extracted blocks after rewriting; locate exact mobility start.
    clm, mob = sections.split('Lch = Leff - Lred', 1)
    args = ','.join(f'{name}=x[{i}]' for i,name in enumerate(NAMES))
    variables = ','.join(map(str, VARIABLES))
    return f'''#include <math.h>
#include <stdio.h>
#define C_ESI 1.034943e-10L
#define C_QE 1.6021918e-19L
#define C_m2cm_p2 1e4L
#define Fn_Pow powl
static void evaluate(int zone,const long double*x,long double*out) {{
 long double {args};
 long double epsm10=2.220446049250313e-15L,small=1e-25L,beta_inv=1/beta,
 q_Nsub=C_QE*nsub,wdpl=sqrtl(2*C_ESI/q_Nsub),mphn1=mphn0*mueph0;
 long double Lred=0.,Psdl=Psl,Lch=0.,Eeff=0.,Muun=0.,Ey=0.,Mu=0.,Ids0=0.;
 {decl}
 long double {scalars};
 if(zone==2 || zone==3) {{ {clm} Lred*=FD2; }}
 Lch = Leff - Lred {mob}
 long double values[]={{Lred,Lch,Psdl,Eeff,Muun,Ey,Mu,Ids0*multiplicity}};
 for(int j=0;j<8;j++)out[j]=values[j];
}}
int main(void) {{
 int zone; long double x[{len(NAMES)}];
 while(scanf("%d",&zone)==1) {{
  for(int j=0;j<{len(NAMES)};j++)if(scanf("%Lf",&x[j])!=1)return 0;
  long double base[8],a[8],b[8];evaluate(zone,x,base);
  for(int j=0;j<8;j++)printf("%.21Lg ",base[j]);
  const int variable[]={{{variables}}};
  for(int axis=0;axis<4;axis++) {{
   long double hi[{len(NAMES)}],lo[{len(NAMES)}],h=1e-5L;
   for(int j=0;j<{len(NAMES)};j++)hi[j]=lo[j]=x[j];
   for(int i=0;i<sizeof(variable)/sizeof(variable[0]);i++) {{
    int j=variable[i];if(j==10 && zone==3)continue;long double d=x[j]*(1+((j+axis*3)%7))/100;
    hi[j]+=h*d;lo[j]-=h*d;
   }}
   evaluate(zone,hi,a);evaluate(zone,lo,b);
   for(int j=0;j<8;j++)printf("%.21Lg ",(a[j]-b[j])/(2*h));
  }}
  putchar(10);
 }}
}}
'''.replace('sqrt(', 'sqrtl(').replace('sqrt (', 'sqrtl (')


def cases():
    for zone, vds, pds, density, length, clm, bb in product(
            (1, 2, 3), (.1, 2., 20.), (.03, .08), (1e-5, .003),
            (1e-6, 8e-6), ((0.,0.), (2.,1.)), (1.,1.6,2.)):
        yield (zone, vds, .25, .25+pds, -.3, density,
               .001, density*.8, density*pds*30, 30., .6,
               {1:0.,2:.43,3:1.}[zone], length, 2e23, .6, *clm, 1.15,
               1e8, 5e7, .1, 1000., 100., .3, 2., 1e15, 5e-5,
               1e5, bb, 5e-6, 3.)


def main():
    root, out = map(Path, sys.argv[1:])
    rows = list(cases())
    with tempfile.TemporaryDirectory(prefix='hisim-transport-oracle-') as tmp, out.open('w') as f:
        f.write('# family zone '+ ' '.join(NAMES) + '; Lred Lch Psdl Eeff Muun Ey Mu Ids0*M and four gradient groups\n')
        for version in (1,2):
            c, exe = Path(tmp)/'oracle.c', Path(tmp)/'oracle'
            c.write_text(program(root, version))
            subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
            result = subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in rows),text=True,capture_output=True,check=True)
            values = result.stdout.splitlines()
            assert len(values)==len(rows)
            for row, value in zip(rows,values):
                assert all(math.isfinite(float(x)) for x in value.split()), (row,value)
                f.write(' '.join(map(str,(version,*row)))+' '+value.strip()+'\n')


if __name__ == '__main__':
    main()
